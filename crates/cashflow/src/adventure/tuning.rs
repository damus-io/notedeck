//! Every constant the XP fold multiplies by, and the quests' targets and
//! rewards, in one place.

/// How much XP each thing the fold counts is worth.
///
/// A "unit" is one whole unit of the home commodity (one CAD, not one
/// cent), so the rates read the way the design states them. The defaults
/// are set so a year of real books lands a skill in the middle levels:
/// OSRS level 50 is about 100k XP and level 70 about 740k.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XpTuning {
    /// Earning XP per unit of income ([`crate::net::NetFlow::income`]).
    pub earning_per_unit: u64,
    /// Saving XP per unit of a month's net, paid when the month closes. A
    /// month that spent more than it earned pays nothing.
    pub saving_per_unit: u64,
    /// Frugality XP per unit a closed month's expenses came in under the
    /// average of the [`Self::frugality_months`] before it.
    pub frugality_per_unit: u64,
    /// How many months before a month its expenses are averaged over, for
    /// Frugality. Fewer are averaged at the journal's start.
    pub frugality_months: u32,
    /// How many sats of net inflow to the BTC asset accounts make one
    /// Stacking XP.
    pub sats_per_xp: u64,
    /// Bookkeeping XP per transaction an override or a split categorizes:
    /// one triaged by hand.
    pub triage_xp: u64,
    /// Bookkeeping XP per transaction a vault rule categorizes.
    pub rule_xp: u64,
    /// How many of a rule's transactions earn [`Self::rule_xp`], so one
    /// broad rule can't level the skill on its own.
    pub rule_cap: u32,
    /// Bookkeeping XP per transaction a transfer link confirms (a link has
    /// two).
    pub link_xp: u64,
    /// Bookkeeping XP per source imported in a month, once a month.
    pub import_xp: u64,
    /// A project skill's XP per unit of that project's revenue.
    pub project_per_unit: u64,
    /// The Well Rested bonus each month of a positive-net streak adds to
    /// Earning and Saving, in percent.
    pub streak_bonus_percent: u32,
    /// The most the Well Rested bonus reaches, in percent.
    pub streak_cap_percent: u32,
    /// How many transactions the weekly triage quest asks for, at most:
    /// fewer when fewer were waiting.
    pub weekly_triage: u32,
    /// Bookkeeping XP for completing a week's triage quest.
    pub weekly_triage_xp: u64,
    /// Saving XP for a closed month whose net beat the month before's.
    pub beat_net_xp: u64,
    /// How many days after its last import a source is stale, and gets an
    /// import quest.
    pub stale_days: u32,
    /// How many days after its last import a source is taken to be gone,
    /// and stops getting one.
    pub forgotten_days: u32,
    /// Bookkeeping XP for importing a stale source.
    pub stale_import_xp: u64,
    /// XP for a goal whose card moved to done, to the skill its metric
    /// levels.
    pub goal_xp: u64,
}

impl Default for XpTuning {
    fn default() -> Self {
        XpTuning {
            earning_per_unit: 10,
            saving_per_unit: 20,
            frugality_per_unit: 10,
            frugality_months: 6,
            sats_per_xp: 1_000,
            triage_xp: 10,
            rule_xp: 5,
            rule_cap: 50,
            link_xp: 5,
            import_xp: 50,
            project_per_unit: 10,
            streak_bonus_percent: 5,
            streak_cap_percent: 25,
            weekly_triage: 50,
            weekly_triage_xp: 250,
            beat_net_xp: 1_000,
            stale_days: 30,
            forgotten_days: 180,
            stale_import_xp: 100,
            goal_xp: 10_000,
        }
    }
}

impl XpTuning {
    /// The Well Rested bonus a month starts with after `streak` positive
    /// months in a row, in percent.
    pub fn bonus_percent(&self, streak: u32) -> u32 {
        streak
            .saturating_mul(self.streak_bonus_percent)
            .min(self.streak_cap_percent)
    }
}

/// `qty` smallest units (at `precision` decimals) times `per_unit` XP per
/// whole unit, raised by `bonus_percent`, rounded down; nothing for a
/// negative `qty`.
pub(crate) fn xp_of(qty: i128, precision: u8, per_unit: u64, bonus_percent: u32) -> u64 {
    if qty <= 0 {
        return 0;
    }
    let scale = 10i128.pow(u32::from(precision));
    let raised = qty * i128::from(per_unit) * (100 + i128::from(bonus_percent));
    u64::try_from(raised / (scale * 100)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// +5% a month, capped at +25%.
    #[test]
    fn bonus_grows_then_caps() {
        let tuning = XpTuning::default();
        assert_eq!(tuning.bonus_percent(0), 0);
        assert_eq!(tuning.bonus_percent(1), 5);
        assert_eq!(tuning.bonus_percent(4), 20);
        assert_eq!(tuning.bonus_percent(5), 25);
        assert_eq!(tuning.bonus_percent(40), 25);
    }

    /// Smallest units scale to whole ones, the bonus raises, and a loss is
    /// nothing.
    #[test]
    fn xp_of_scales_and_raises() {
        // CAD 421.00 at 10 XP a dollar.
        assert_eq!(xp_of(42_100, 2, 10, 0), 4_210);
        assert_eq!(xp_of(42_100, 2, 10, 25), 5_262);
        assert_eq!(xp_of(99, 2, 1, 0), 0);
        assert_eq!(xp_of(-42_100, 2, 10, 0), 0);
    }
}
