//! The Usage section's state: where the context window stands, what the
//! session has cost, and how both moved turn by turn.
//!
//! [`UsageHistory`] folds three responses:
//!
//! - `UsageUpdate` — one API request's token counts, sent per assistant
//!   message. The latest is the *live* row: the context as it stands now.
//! - `QueryComplete` — the end of a turn. It closes the live row into a
//!   [`TurnUsage`] and carries the session's cost so far.
//! - `CompactionComplete` — the CLI summarised the conversation. It becomes a
//!   [`Compaction`] marker between two turns; the next `UsageUpdate` says how
//!   big the context was afterwards.
//!
//! The history is bounded ([`HISTORY_CAP`] turns) and starts empty: a restored
//! session has no history until its next turn ends.

use std::collections::VecDeque;

use crate::messages::{CompactionInfo, DaveApiResponse, UsageInfo};
use agentium_core::inspector_snapshot::UsageSnapshot;
pub use agentium_core::inspector_snapshot::{Compaction, Tokens, TurnUsage};

/// Most turns the history keeps. Older turns drop off the front.
pub const HISTORY_CAP: usize = 500;

/// The context-per-turn sparkline, rebuilt whenever the history changes so
/// the card only maps it to screen space.
#[derive(Debug, Default)]
pub struct Sparkline {
    /// Context tokens at the end of each retained turn, oldest first.
    pub values: Vec<f32>,
    /// Largest value, for the y scale.
    pub max: f32,
    /// Where each retained compaction falls, as a fractional index into
    /// `values`: halfway between the turn before it and the turn after.
    pub markers: Vec<f32>,
}

/// The Usage section's state. See the module docs.
#[derive(Debug, Default)]
pub struct UsageHistory {
    /// Retained turns, oldest first, at most [`HISTORY_CAP`].
    turns: VecDeque<TurnUsage>,
    /// Every turn ever recorded, including the ones the cap dropped.
    turns_recorded: u64,
    /// Model requests across those turns (the sum of `num_turns`).
    requests: u64,
    /// Compactions, oldest first, at most [`HISTORY_CAP`].
    compactions: VecDeque<Compaction>,
    /// The latest request's tokens: the context right now.
    current: Option<Tokens>,
    /// A `UsageUpdate` arrived since the last turn ended. When it hasn't (a
    /// backend that only reports at turn end), the turn takes the
    /// `QueryComplete`'s own counts.
    live: bool,
    /// The cumulative cost the backend last reported, to take each turn's
    /// share from.
    reported_cost: Option<f64>,
    /// The session's cost: the sum of every turn's share.
    total_cost: Option<f64>,
    /// Bumped on every change, so display strings built from the history know
    /// when to rebuild.
    generation: u64,
    sparkline: Sparkline,
}

impl UsageHistory {
    /// Fold one backend response, stamping anything it records with `at`
    /// (Unix seconds).
    pub fn apply(&mut self, res: &DaveApiResponse, at: u64) {
        match res {
            DaveApiResponse::UsageUpdate(usage) => self.request(usage),
            DaveApiResponse::QueryComplete(usage) => self.turn_ended(usage, at),
            DaveApiResponse::CompactionComplete(info) => self.compacted(info, at),
            _ => return,
        }
        self.generation += 1;
    }

    /// A request finished: it is the live context now, and the first one
    /// after a compaction says how far the compaction shrank it.
    fn request(&mut self, usage: &UsageInfo) {
        let tokens = Tokens::from_usage(usage);
        self.current = Some(tokens);
        self.live = true;
        if let Some(last) = self.compactions.back_mut() {
            if last.post_tokens.is_none() {
                last.post_tokens = Some(tokens.context());
            }
        }
    }

    /// A turn ended: close the live row into the history.
    fn turn_ended(&mut self, usage: &UsageInfo, at: u64) {
        let tokens = match self.current {
            Some(tokens) if self.live => tokens,
            _ => Tokens::from_usage(usage),
        };
        self.current = Some(tokens);
        self.live = false;

        let cost_usd = usage.cost_usd.map(|reported| self.cost_share(reported));
        if let Some(cost) = cost_usd {
            *self.total_cost.get_or_insert(0.0) += cost;
        }

        self.turns.push_back(TurnUsage {
            at,
            tokens,
            cost_usd,
        });
        if self.turns.len() > HISTORY_CAP {
            self.turns.pop_front();
        }
        self.turns_recorded += 1;
        self.requests += u64::from(usage.num_turns);
        self.rebuild_sparkline();
    }

    /// This turn's share of a cumulative cost report. The CLI reports its
    /// process's running total, which starts over when a new process isn't
    /// resumed with its old total; a total that went down is taken as a
    /// fresh start.
    fn cost_share(&mut self, reported: f64) -> f64 {
        let share = match self.reported_cost {
            Some(previous) if reported >= previous => reported - previous,
            _ => reported,
        };
        self.reported_cost = Some(reported);
        share
    }

    fn compacted(&mut self, info: &CompactionInfo, at: u64) {
        self.compactions.push_back(Compaction {
            at,
            turn: self.turns_recorded,
            pre_tokens: info.pre_tokens,
            post_tokens: None,
        });
        if self.compactions.len() > HISTORY_CAP {
            self.compactions.pop_front();
        }
        self.rebuild_sparkline();
    }

    fn rebuild_sparkline(&mut self) {
        let first = self.first_retained_turn();
        let spark = &mut self.sparkline;
        spark.values.clear();
        spark
            .values
            .extend(self.turns.iter().map(|t| t.tokens.context() as f32));
        spark.max = spark.values.iter().copied().fold(0.0, f32::max);
        spark.markers.clear();
        spark.markers.extend(
            self.compactions
                .iter()
                .filter(|c| c.turn > first)
                .map(|c| (c.turn - first) as f32 - 0.5),
        );
    }

    /// The session-wide number of the oldest retained turn.
    fn first_retained_turn(&self) -> u64 {
        self.turns_recorded - self.turns.len() as u64
    }

    /// Replace the history with a host's snapshot of it, for a remote
    /// session. Its cost reports are the host's, so none is carried over to
    /// take a share from.
    pub fn restore(&mut self, snapshot: UsageSnapshot) {
        let UsageSnapshot {
            current,
            turns,
            compactions,
            turns_recorded,
            requests,
            total_cost,
        } = snapshot;
        self.turns = turns.into();
        self.compactions = compactions.into();
        // A snapshot from a host that cut its turns still counts them all,
        // but never fewer than it carries.
        self.turns_recorded = turns_recorded.max(self.turns.len() as u64);
        self.requests = requests;
        self.current = current;
        self.live = false;
        self.reported_cost = None;
        self.total_cost = total_cost;
        self.generation += 1;
        self.rebuild_sparkline();
    }

    /// Retained turns, oldest first.
    pub fn turns(&self) -> &VecDeque<TurnUsage> {
        &self.turns
    }

    /// Compactions, oldest first.
    pub fn compactions(&self) -> &VecDeque<Compaction> {
        &self.compactions
    }

    /// The latest request's tokens: the context right now.
    pub fn current(&self) -> Option<Tokens> {
        self.current
    }

    /// Every turn the session recorded, including dropped ones.
    pub fn turns_recorded(&self) -> u64 {
        self.turns_recorded
    }

    /// Model requests across every recorded turn.
    pub fn requests(&self) -> u64 {
        self.requests
    }

    /// What the session has cost, if the backend reports cost.
    pub fn total_cost(&self) -> Option<f64> {
        self.total_cost
    }

    /// Changes with every fold that changed anything.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn sparkline(&self) -> &Sparkline {
        &self.sparkline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(input: u64, cache_creation: u64, cache_read: u64, output: u64) -> DaveApiResponse {
        DaveApiResponse::UsageUpdate(UsageInfo {
            input_tokens: input,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            output_tokens: output,
            ..Default::default()
        })
    }

    fn turn_end(cost: f64, num_turns: u32) -> DaveApiResponse {
        DaveApiResponse::QueryComplete(UsageInfo {
            // The result's own counts are the turn's totals, not the context;
            // a turn with live requests must not take them.
            input_tokens: 999_999,
            cost_usd: Some(cost),
            num_turns,
            ..Default::default()
        })
    }

    /// Requests update the live row, a turn end closes it, a compaction lands
    /// between turns and learns its size after from the next request.
    #[test]
    fn folds_turns_and_compactions() {
        let mut usage = UsageHistory::default();
        usage.apply(&request(10, 1_000, 0, 50), 1);
        usage.apply(&request(20, 100, 1_000, 80), 2);
        assert_eq!(usage.current().map(|t| t.context()), Some(1_120));
        assert!(usage.turns().is_empty(), "no turn has ended yet");

        usage.apply(&turn_end(0.25, 2), 3);
        usage.apply(&request(5, 0, 150_000, 10), 4);
        usage.apply(&turn_end(0.75, 1), 5);
        usage.apply(
            &DaveApiResponse::CompactionComplete(CompactionInfo {
                pre_tokens: 150_005,
            }),
            6,
        );
        usage.apply(&request(30_000, 0, 0, 20), 7);
        usage.apply(&turn_end(1.0, 1), 8);

        let turns: Vec<_> = usage.turns().iter().map(|t| t.tokens.context()).collect();
        assert_eq!(turns, [1_120, 150_005, 30_000]);
        assert_eq!(usage.turns()[0].tokens.output, 80, "the last request's");
        assert_eq!(usage.turns()[2].at, 8);

        let costs: Vec<_> = usage.turns().iter().map(|t| t.cost_usd).collect();
        assert_eq!(costs, [Some(0.25), Some(0.5), Some(0.25)]);
        assert_eq!(usage.total_cost(), Some(1.0));
        assert_eq!(usage.requests(), 4);

        let compaction = usage.compactions()[0];
        assert_eq!(compaction.turn, 2, "after the second turn");
        assert_eq!(compaction.pre_tokens, 150_005);
        assert_eq!(compaction.post_tokens, Some(30_000));
        assert_eq!(usage.sparkline().values, [1_120.0, 150_005.0, 30_000.0]);
        assert_eq!(usage.sparkline().max, 150_005.0);
        assert_eq!(usage.sparkline().markers, [1.5]);
    }

    /// A backend that reports only at turn end (no `UsageUpdate`) records the
    /// turn's own counts, and one that reports no cost records none.
    #[test]
    fn a_turn_without_requests_takes_the_turn_end_counts() {
        let mut usage = UsageHistory::default();
        usage.apply(
            &DaveApiResponse::QueryComplete(UsageInfo {
                input_tokens: 4_000,
                output_tokens: 300,
                ..Default::default()
            }),
            1,
        );
        let turn = usage.turns()[0];
        assert_eq!(turn.tokens.context(), 4_000);
        assert_eq!(turn.tokens.output, 300);
        assert_eq!(turn.cost_usd, None);
        assert_eq!(usage.total_cost(), None);
    }

    /// A cumulative cost that goes down means a new CLI process started from
    /// zero: its report is all new spend.
    #[test]
    fn a_cost_that_restarts_counts_from_zero() {
        let mut usage = UsageHistory::default();
        usage.apply(&turn_end(2.0, 1), 1);
        usage.apply(&turn_end(0.5, 1), 2);
        usage.apply(&turn_end(0.75, 1), 3);
        let costs: Vec<_> = usage.turns().iter().map(|t| t.cost_usd).collect();
        assert_eq!(costs, [Some(2.0), Some(0.5), Some(0.25)]);
        assert_eq!(usage.total_cost(), Some(2.75));
    }

    /// The history holds the newest 500 turns; the count, the cost and the
    /// compaction markers still account for the dropped ones.
    #[test]
    fn the_cap_holds_at_500() {
        let mut usage = UsageHistory::default();
        for turn in 0..(HISTORY_CAP as u64 + 20) {
            if turn == 10 || turn == 30 {
                usage.apply(
                    &DaveApiResponse::CompactionComplete(CompactionInfo { pre_tokens: 1 }),
                    turn,
                );
            }
            usage.apply(&request(turn, 0, 0, 0), turn);
            usage.apply(&turn_end((turn + 1) as f64, 1), turn);
        }

        assert_eq!(usage.turns().len(), HISTORY_CAP);
        assert_eq!(usage.turns_recorded(), 520);
        assert_eq!(usage.turns().front().unwrap().tokens.input, 20);
        assert_eq!(usage.total_cost(), Some(520.0));
        assert_eq!(usage.sparkline().values.len(), HISTORY_CAP);
        // The compaction before turn 10 fell off with its turns; the one
        // before turn 30 is now just before retained turn 10.
        assert_eq!(usage.compactions().len(), 2);
        assert_eq!(usage.sparkline().markers, [9.5]);
    }
}
