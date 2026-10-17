//! Exchange rates from `P` directives, and conversion at a date.
//!
//! A [`PriceDb`] turns a ledger's prices into a graph whose nodes are
//! commodities and whose edges are the pairs some `P` directive relates, each
//! edge holding its rates in date order. A lookup for one commodity in
//! another at a date:
//!
//! - **Never sees the future.** Each hop uses its most recent rate on or
//!   before the date, and an edge whose first rate is later than the date is
//!   not in the graph at all.
//! - **Chains.** It takes the shortest path in hops (`msat → sat → BTC → USD
//!   → CAD`), found by breadth-first search and memoized per pair and per set
//!   of edges that exist by the date. Among equally short paths it takes the
//!   one through the edges that started earliest, which depends only on the
//!   prices and the commodity names, never on load order.
//! - **Inverts.** `P 2024-01-02 USD 1.35 CAD` prices CAD in USD too, as
//!   `1 / 1.35`.
//! - **Is exact.** A [`Rate`] is a reduced fraction of two `i128`s, and a
//!   chain multiplies fractions; nothing rounds until [`PriceDb::convert`]
//!   produces a quantity in the target's smallest unit, and that rounds once,
//!   to the nearest unit, halves away from zero.
//! - **Never guesses.** No path, or a product too large for an `i128`,
//!   returns `None`. [`Unpriced`] tallies those misses so a view can say how
//!   much it could not price, rather than show a wrong number.
//!
//! When several prices for one pair share a date, the one that comes last in
//! the slice the db was built from wins; in a [`Ledger`] that is the one from
//! the segment with the latest `imported` header, then the largest hash, so a
//! re-fetched rate replaces the one it revises and the winner never depends
//! on load order.
//! A price of zero or less cannot be inverted or chained and is ignored.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use crate::{Amount, Commodities, CommodityId, Date, Decimal, Ledger, Price};

/// An exact, positive exchange rate: one unit of a commodity is worth
/// `num / den` units of another. Always reduced, so equal rates compare
/// equal.
///
/// Units here are whole units as written (`1 BTC`, `1 CAD`), not the
/// smallest units amounts are stored in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rate {
    num: i128,
    den: i128,
}

impl Rate {
    /// The rate of a commodity in itself.
    pub const ONE: Rate = Rate { num: 1, den: 1 };

    /// `num / den`, reduced, or `None` unless both are positive.
    pub fn new(num: i128, den: i128) -> Option<Rate> {
        if num <= 0 || den <= 0 {
            return None;
        }
        let g = gcd(num, den);
        Some(Rate {
            num: num / g,
            den: den / g,
        })
    }

    /// The rate a price's value states, or `None` unless it is positive.
    pub fn from_decimal(value: Decimal) -> Option<Rate> {
        let den = 10i128.checked_pow(u32::from(value.scale))?;
        Rate::new(i128::from(value.mantissa), den)
    }

    /// The numerator, in lowest terms.
    pub fn num(self) -> i128 {
        self.num
    }

    /// The denominator, in lowest terms; always positive.
    pub fn den(self) -> i128 {
        self.den
    }

    /// The rate the other way round.
    pub fn inverse(self) -> Rate {
        Rate {
            num: self.den,
            den: self.num,
        }
    }

    /// `self × other`, exact, or `None` when the reduced product does not
    /// fit in an `i128`.
    pub fn checked_mul(self, other: Rate) -> Option<Rate> {
        // Cancel across before multiplying so the product only overflows
        // when its lowest terms do.
        let a = gcd(self.num, other.den);
        let b = gcd(other.num, self.den);
        Some(Rate {
            num: (self.num / a).checked_mul(other.num / b)?,
            den: (self.den / b).checked_mul(other.den / a)?,
        })
    }
}

/// One rate on one edge, as of a date.
#[derive(Copy, Clone, Debug)]
struct Quote {
    date: Date,
    /// One unit of the edge's `a` in `b`.
    rate: Rate,
}

/// Two commodities some prices relate, and those prices in date order, one
/// per date.
#[derive(Debug)]
struct Edge {
    a: CommodityId,
    b: CommodityId,
    quotes: Vec<Quote>,
}

impl Edge {
    /// The day the edge's first rate applies from.
    fn first(&self) -> Date {
        self.quotes[0].date
    }

    /// One unit of `a` in `b` on `date`: the most recent rate on or before
    /// it.
    fn rate_at(&self, date: Date) -> Option<Rate> {
        let after = self.quotes.partition_point(|q| q.date <= date);
        Some(self.quotes.get(after.checked_sub(1)?)?.rate)
    }
}

/// An edge leaving a commodity, from that commodity's side.
#[derive(Copy, Clone, Debug)]
struct Neighbor {
    /// Index into [`PriceDb::edges`].
    edge: u32,
    /// The commodity at the other end.
    to: CommodityId,
}

/// One step of a path: an edge, walked from `a` to `b` or back.
#[derive(Copy, Clone, Debug)]
struct Hop {
    edge: u32,
    forward: bool,
}

/// What a path lookup is memoized on: the pair, and how many edges exist by
/// the date (edges are sorted by first date, so that count names the graph).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct PathKey {
    from: CommodityId,
    to: CommodityId,
    edges: u32,
}

/// The two ends of an edge, smaller id first, so either direction finds it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct Pair {
    lo: CommodityId,
    hi: CommodityId,
}

/// Exchange rates from a set of `P` directives, for converting amounts at
/// the date they happened. See the module docs for the rules.
///
/// Its commodity ids are the ones of the table it was built with; rebuild it
/// whenever the ledger's prices or precisions change.
#[derive(Debug)]
pub struct PriceDb {
    /// Edges sorted by first date, then by the two commodities' names.
    edges: Vec<Edge>,
    /// Each commodity's edges, by commodity id, in edge order.
    adjacency: Vec<Vec<Neighbor>>,
    /// Each commodity's precision, by commodity id.
    precisions: Vec<u8>,
    /// Shortest paths found so far; `None` is a pair with no path.
    paths: RefCell<HashMap<PathKey, Option<Box<[Hop]>>>>,
}

impl PriceDb {
    /// The rates in `ledger`'s prices, for its commodities.
    pub fn from_ledger(ledger: &Ledger) -> PriceDb {
        PriceDb::new(ledger.prices(), &ledger.journal().commodities)
    }

    /// The rates in `prices`, whose commodities are in `commodities`. On a
    /// date with several prices for one pair, the last in `prices` wins.
    #[profiling::function]
    pub fn new(prices: &[Price], commodities: &Commodities) -> PriceDb {
        let mut by_pair: HashMap<Pair, Vec<Quote>> = HashMap::new();
        for price in prices {
            if price.commodity == price.quote {
                continue;
            }
            let Some(rate) = Rate::from_decimal(price.value) else {
                continue;
            };
            let (pair, rate) = if price.commodity < price.quote {
                let pair = Pair {
                    lo: price.commodity,
                    hi: price.quote,
                };
                (pair, rate)
            } else {
                let pair = Pair {
                    lo: price.quote,
                    hi: price.commodity,
                };
                (pair, rate.inverse())
            };
            by_pair.entry(pair).or_default().push(Quote {
                date: price.date,
                rate,
            });
        }

        let mut edges: Vec<Edge> = by_pair
            .into_iter()
            .map(|(pair, mut quotes)| {
                // Stable, so among same-date quotes the slice's last stays
                // last, and dedup keeps it.
                quotes.sort_by_key(|q| q.date);
                quotes.dedup_by(|later, earlier| {
                    let same = later.date == earlier.date;
                    if same {
                        *earlier = *later;
                    }
                    same
                });
                Edge {
                    a: pair.lo,
                    b: pair.hi,
                    quotes,
                }
            })
            .collect();
        edges.sort_by(|x, y| {
            x.first().cmp(&y.first()).then_with(|| {
                let names = |e: &Edge| sorted_names(commodities, e.a, e.b);
                names(x).cmp(&names(y))
            })
        });

        let mut adjacency = vec![Vec::new(); commodities.len()];
        for (at, edge) in edges.iter().enumerate() {
            let at = at as u32;
            adjacency[edge.a.index()].push(Neighbor {
                edge: at,
                to: edge.b,
            });
            adjacency[edge.b.index()].push(Neighbor {
                edge: at,
                to: edge.a,
            });
        }

        PriceDb {
            edges,
            adjacency,
            precisions: commodities
                .ids()
                .map(|c| commodities.precision(c))
                .collect(),
            paths: RefCell::new(HashMap::new()),
        }
    }

    /// Whether the db holds no usable prices.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// What one unit of `from` was worth in `to` on `date`, chaining through
    /// other commodities if it must, or `None` when no prices on or before
    /// `date` connect them (or the exact chain overflows).
    pub fn rate(&self, from: CommodityId, to: CommodityId, date: Date) -> Option<Rate> {
        if from == to {
            return Some(Rate::ONE);
        }
        if from.index() >= self.adjacency.len() || to.index() >= self.adjacency.len() {
            return None;
        }
        let key = PathKey {
            from,
            to,
            edges: self.edges.partition_point(|e| e.first() <= date) as u32,
        };
        let mut paths = self.paths.borrow_mut();
        let path = paths.entry(key).or_insert_with(|| self.shortest_path(key));
        let mut rate = Rate::ONE;
        for hop in path.as_deref()? {
            let edge = self.edges[hop.edge as usize].rate_at(date)?;
            let step = if hop.forward { edge } else { edge.inverse() };
            rate = rate.checked_mul(step)?;
        }
        Some(rate)
    }

    /// `amount` in commodity `to` at the rate of `date`, rounded to the
    /// nearest unit of `to`'s precision with halves away from zero, or
    /// `None` when there is no rate or the result does not fit.
    pub fn convert(&self, amount: Amount, to: CommodityId, date: Date) -> Option<Amount> {
        let rate = self.rate(amount.commodity, to, date)?;
        self.convert_at(amount, to, rate)
    }

    /// [`Self::convert`] at a rate already looked up with [`Self::rate`],
    /// for a caller converting many amounts on the same day.
    pub(crate) fn convert_at(&self, amount: Amount, to: CommodityId, rate: Rate) -> Option<Amount> {
        let from_precision = *self.precisions.get(amount.commodity.index())?;
        let to_precision = *self.precisions.get(to.index())?;

        // qty / 10^pf whole units × num/den, in units of 10^-pt:
        // qty × num × 10^pt / (den × 10^pf).
        let qty = i128::from(amount.qty);
        let g = gcd(qty, rate.den);
        let mut n = (qty / g).checked_mul(rate.num)?;
        let mut d = rate.den / g;
        match to_precision.cmp(&from_precision) {
            Ordering::Greater => n = n.checked_mul(pow10(to_precision - from_precision)?)?,
            Ordering::Less => d = d.checked_mul(pow10(from_precision - to_precision)?)?,
            Ordering::Equal => {}
        }
        let qty = i64::try_from(div_round(n, d)).ok()?;
        Some(Amount { qty, commodity: to })
    }

    /// [`Self::convert`], recording a miss in `unpriced`.
    pub fn convert_counting(
        &self,
        amount: Amount,
        to: CommodityId,
        date: Date,
        unpriced: &mut Unpriced,
    ) -> Option<Amount> {
        let converted = self.convert(amount, to, date);
        if converted.is_none() {
            unpriced.record(amount.commodity, to, date);
        }
        converted
    }

    /// The fewest-hop path from `key.from` to `key.to` over the first
    /// `key.edges` edges.
    fn shortest_path(&self, key: PathKey) -> Option<Box<[Hop]>> {
        let mut came_by: Vec<Option<u32>> = vec![None; self.adjacency.len()];
        let mut seen = vec![false; self.adjacency.len()];
        let mut queue = std::collections::VecDeque::new();
        seen[key.from.index()] = true;
        queue.push_back(key.from);
        while let Some(at) = queue.pop_front() {
            if at == key.to {
                break;
            }
            for next in &self.adjacency[at.index()] {
                if next.edge >= key.edges || seen[next.to.index()] {
                    continue;
                }
                seen[next.to.index()] = true;
                came_by[next.to.index()] = Some(next.edge);
                queue.push_back(next.to);
            }
        }
        if !seen[key.to.index()] {
            return None;
        }

        let mut hops = Vec::new();
        let mut at = key.to;
        while at != key.from {
            let edge = came_by[at.index()].expect("every reached commodity but the start has one");
            let e = &self.edges[edge as usize];
            let forward = e.b == at;
            hops.push(Hop { edge, forward });
            at = if forward { e.a } else { e.b };
        }
        hops.reverse();
        Some(hops.into_boxed_slice())
    }
}

/// Conversions that found no rate, so a view can badge what it left out
/// instead of showing a wrong total.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unpriced {
    /// Every miss.
    pub count: u64,
    /// The misses by pair, in the order each pair first missed.
    pub gaps: Vec<PriceGap>,
}

/// The misses converting one commodity into another.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PriceGap {
    /// The commodity that could not be priced.
    pub from: CommodityId,
    /// The commodity it was being converted into.
    pub to: CommodityId,
    /// How many conversions missed.
    pub count: u64,
    /// The earliest date that missed.
    pub first: Date,
    /// The latest date that missed.
    pub last: Date,
}

impl Unpriced {
    /// Count a conversion from `from` into `to` on `date` that found no rate.
    pub fn record(&mut self, from: CommodityId, to: CommodityId, date: Date) {
        self.count += 1;
        if let Some(gap) = self.gaps.iter_mut().find(|g| g.from == from && g.to == to) {
            gap.count += 1;
            gap.first = gap.first.min(date);
            gap.last = gap.last.max(date);
            return;
        }
        self.gaps.push(PriceGap {
            from,
            to,
            count: 1,
            first: date,
            last: date,
        });
    }

    /// Whether nothing missed.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// The names of `a` and `b`, alphabetically, so an edge sorts the same
/// whichever way round its price was written.
fn sorted_names(commodities: &Commodities, a: CommodityId, b: CommodityId) -> [&str; 2] {
    let mut names = [commodities.name(a), commodities.name(b)];
    names.sort_unstable();
    names
}

/// The greatest common divisor of `|a|` and `b`, where `b > 0`.
fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.unsigned_abs(), b.unsigned_abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    // At most |b| of the inputs, which was a positive i128.
    a as i128
}

/// `10^exp`, or `None` past `i128`.
fn pow10(exp: u8) -> Option<i128> {
    10i128.checked_pow(u32::from(exp))
}

/// `n / d` rounded to the nearest integer, halves away from zero; `d > 0`.
fn div_round(n: i128, d: i128) -> i128 {
    let q = n / d;
    let r = (n % d).abs();
    // r ≥ d - r is 2r ≥ d without overflowing.
    if r >= d - r { q + n.signum() } else { q }
}
