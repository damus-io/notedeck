//! What a model's tokens cost, so a run can stop at its spec's
//! `budget.max_cost_cents`.
//!
//! ```text
//! response.usage { prompt_tokens, completion_tokens }
//!        │                      Rate (by the request's model id)
//!        ▼                          │
//!     Usage ──────── Rate::cost ────┘──▶ Cost (micro-cents) ──▶ Meter
//! ```
//!
//! The table is small and kept by hand: the models the built-in specs and
//! the default native model run on. A model it doesn't list has no
//! [`Rate`], and a run on it is never stopped for cost: its spend is
//! unknown, not zero.
//!
//! Every input token is charged at the full input rate. A provider bills
//! cached prompt tokens for less, so a run's spend reads high, never low:
//! it stops early rather than late.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// One request's token counts, as the provider reported them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Tokens sent: the prompt, cached or not.
    pub input_tokens: u64,
    /// Tokens the model wrote.
    pub output_tokens: u64,
}

/// A rate card: US cents per million tokens, each way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    /// Cents per million input tokens.
    pub input: u64,
    /// Cents per million output tokens.
    pub output: u64,
}

/// A model's price, which can depend on how long the prompt is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// The price of a prompt up to [`LongPrompt::above_tokens`], or of any
    /// prompt when there is no long-prompt price.
    pub base: Price,
    /// A second price for long prompts.
    pub long: Option<LongPrompt>,
}

/// The price a model charges once a prompt passes a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongPrompt {
    /// A prompt longer than this many tokens is charged [`Self::price`].
    pub above_tokens: u64,
    /// The price of the whole request then.
    pub price: Price,
}

impl Rate {
    /// What `usage` cost.
    pub fn cost(&self, usage: Usage) -> Cost {
        let price = match self.long {
            Some(long) if usage.input_tokens > long.above_tokens => long.price,
            _ => self.base,
        };
        // Cents per million tokens is micro-cents per token.
        Cost::from_microcents(usage.input_tokens * price.input + usage.output_tokens * price.output)
    }
}

/// One row of the table: a model id and its rate.
struct ModelRate {
    model: &'static str,
    rate: Rate,
}

/// Model ids and their rates, as of 2026-11.
const RATES: &[ModelRate] = &[
    flat("claude-fable-5-2", 800, 4000),
    flat("claude-opus-5-5", 400, 2000),
    flat("claude-sonnet-5-5", 200, 1000),
    ModelRate {
        model: "claude-haiku-5-5",
        rate: Rate {
            base: Price {
                input: 10,
                output: 50,
            },
            long: Some(LongPrompt {
                above_tokens: 100_000,
                price: Price {
                    input: 50,
                    output: 250,
                },
            }),
        },
    },
    flat("gpt-4.1-mini", 40, 160),
];

/// `model` at one price whatever the prompt's length.
const fn flat(model: &'static str, input: u64, output: u64) -> ModelRate {
    ModelRate {
        model,
        rate: Rate {
            base: Price { input, output },
            long: None,
        },
    }
}

/// The rate of the model `model` (its id, as a request names it); `None`
/// for one the table doesn't list.
pub fn rate(model: &str) -> Option<Rate> {
    RATES
        .iter()
        .find(|row| row.model == model)
        .map(|row| row.rate)
}

/// An amount spent, in millionths of a US cent: small enough that one token
/// of the cheapest model is a whole number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cost {
    microcents: u64,
}

impl Cost {
    /// Nothing spent.
    pub const ZERO: Cost = Cost { microcents: 0 };

    /// `microcents` millionths of a cent.
    pub const fn from_microcents(microcents: u64) -> Self {
        Self { microcents }
    }

    /// `cents` cents, as a spend is written in a tool answer; a negative or
    /// non-finite amount is nothing.
    pub fn from_cents(cents: f64) -> Self {
        if !cents.is_finite() || cents <= 0.0 {
            return Self::ZERO;
        }
        Self::from_microcents((cents * 1_000_000.0).round() as u64)
    }

    /// In cents.
    pub fn cents(self) -> f64 {
        self.microcents as f64 / 1_000_000.0
    }

    /// In US dollars.
    pub fn usd(self) -> f64 {
        self.cents() / 100.0
    }

    /// Whether it is nothing.
    pub fn is_zero(self) -> bool {
        self.microcents == 0
    }

    /// Whether it has reached `cents` (a budget's `max_cost_cents`).
    pub fn reached(self, cents: u32) -> bool {
        self.microcents >= u64::from(cents) * 1_000_000
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;

    fn add(self, other: Cost) -> Cost {
        Cost::from_microcents(self.microcents.saturating_add(other.microcents))
    }
}

impl fmt::Display for Cost {
    /// In dollars to the cent, as `$1.23`; a spend under a cent is `<$0.01`
    /// rather than a misleading `$0.00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_zero() && self.cents() < 1.0 {
            return f.write_str("<$0.01");
        }
        write!(f, "${:.2}", self.usd())
    }
}

/// A running total that requests add to from wherever they finish: a
/// stream's task, a sub-agent's thread.
#[derive(Debug, Default)]
pub struct Meter {
    microcents: AtomicU64,
}

impl Meter {
    /// Count `cost`.
    pub fn add(&self, cost: Cost) {
        self.microcents
            .fetch_add(cost.microcents, Ordering::Relaxed);
    }

    /// Raise the total to `cost` if it is below it: for a figure that is
    /// re-read whole each time and only grows, like a run's sub-agent spend.
    pub fn raise_to(&self, cost: Cost) {
        self.microcents
            .fetch_max(cost.microcents, Ordering::Relaxed);
    }

    /// Everything counted so far.
    pub fn spent(&self) -> Cost {
        Cost::from_microcents(self.microcents.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_prices_both_directions_per_million_tokens() {
        let sonnet = rate("claude-sonnet-5-5").unwrap();
        let cost = sonnet.cost(Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
        });
        // $2 in, $1 out.
        assert_eq!(cost.cents(), 300.0);
        assert_eq!(cost.to_string(), "$3.00");
    }

    #[test]
    fn a_long_prompt_is_charged_its_own_price() {
        let haiku = rate("claude-haiku-5-5").unwrap();
        let short = haiku.cost(Usage {
            input_tokens: 100_000,
            output_tokens: 0,
        });
        let long = haiku.cost(Usage {
            input_tokens: 100_001,
            output_tokens: 0,
        });
        assert_eq!(short.cents(), 1.0);
        assert!(long.cents() > 5.0, "{long:?}");
    }

    #[test]
    fn an_unlisted_model_has_no_rate() {
        assert_eq!(rate("qwen3"), None);
    }

    #[test]
    fn a_cost_reaches_a_budget_at_its_last_microcent() {
        let budget = 2;
        assert!(!Cost::from_microcents(1_999_999).reached(budget));
        assert!(Cost::from_microcents(2_000_000).reached(budget));
        assert_eq!(Cost::from_cents(150.0).to_string(), "$1.50");
        assert_eq!(Cost::from_cents(0.004).to_string(), "<$0.01");
        assert_eq!(Cost::ZERO.to_string(), "$0.00");
        assert_eq!(Cost::from_cents(-3.0), Cost::ZERO);
    }

    #[test]
    fn a_meter_adds_what_it_is_given() {
        let meter = Meter::default();
        meter.add(Cost::from_cents(1.25));
        meter.add(Cost::from_cents(0.75));
        assert_eq!(meter.spent(), Cost::from_cents(2.0));
        meter.raise_to(Cost::from_cents(1.0));
        assert_eq!(meter.spent(), Cost::from_cents(2.0), "never lowered");
        meter.raise_to(Cost::from_cents(3.0));
        assert_eq!(meter.spent(), Cost::from_cents(3.0));
    }
}
