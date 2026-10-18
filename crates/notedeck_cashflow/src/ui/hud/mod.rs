//! The Adventure view's game HUD: tooltips, XP bars, skill tiles and toasts.
//!
//! Style-inspired by MMO HUDs (dark glass panels in gilt hairlines, bold
//! gold titles, colored "rarity" lines, a notched XP bar, a skills-tab tile
//! with its level on a diagonal, a gilded banner toast), but every shape is
//! painter-drawn and every color comes from [`HudPalette`]: no game's assets,
//! fonts or names. A second theme reskins the HUD by building a different
//! palette. Titles and levels are set in notedeck's own bold and medium faces
//! ([`Faces`]).
//!
//! The widgets take what they show as borrowed `&str`s and plain numbers, and
//! hold no state of their own except [`Toasts`] and [`FloatingXp`], which own
//! their queues. Numbers
//! are formatted straight into the one `String` egui needs to lay text out
//! (through `format_args!` and [`Grouped`]), never into an intermediate
//! buffer, so a widget adds no allocation beyond egui's own per text run.
//!
//! [`HudPalette`]: crate::ui::palette::HudPalette
//! [`Faces`]: crate::ui::palette::Faces

mod burst;
mod coin;
mod float;
mod paint;
mod skill_tile;
mod toast;
mod tooltip;
mod xp_bar;

pub use burst::{BURST, BurstPhase, paint_burst};
pub use float::{FloatPhase, FloatingXp};
pub use skill_tile::{SKILL_TILE_SIZE, locked_skill_tile, skill_tile};
pub use toast::{Toast, ToastKind, ToastPhase, Toasts};
pub use tooltip::{HudTooltip, TOOLTIP_MAX_WIDTH, show_tooltip, tooltip_frame, tooltip_ui};
pub use xp_bar::{XP_BAR_HEIGHT, XP_BAR_SEGMENTS, XP_BAR_SLIM_HEIGHT, xp_bar, xp_bar_slim};

use std::fmt;

use egui::Color32;

use super::palette::HudPalette;

/// The highest level a skill reaches. A skill at it shows a full bar.
pub const MAX_LEVEL: u8 = 99;

/// How notable a number is, from everyday to rare, drawn as the color of its
/// tooltip line: body text, then green, blue, purple and orange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rarity {
    /// An ordinary number, in body text.
    Common,
    /// Green.
    Uncommon,
    /// Blue.
    Rare,
    /// Purple.
    Epic,
    /// Orange.
    Legendary,
}

impl Rarity {
    /// The tier `value` reaches, given the thresholds at which it becomes
    /// uncommon, rare, epic and legendary (ascending). Below the first it is
    /// common.
    pub fn tier(value: f64, thresholds: &[f64; 4]) -> Self {
        const TIERS: [Rarity; 4] = [
            Rarity::Uncommon,
            Rarity::Rare,
            Rarity::Epic,
            Rarity::Legendary,
        ];
        let mut tier = Rarity::Common;
        for (threshold, next) in thresholds.iter().zip(TIERS) {
            if value < *threshold {
                break;
            }
            tier = next;
        }
        tier
    }

    /// This tier's color in `hud`.
    pub fn color(self, hud: &HudPalette) -> Color32 {
        match self {
            Rarity::Common => hud.text,
            Rarity::Uncommon => hud.uncommon,
            Rarity::Rare => hud.rare,
            Rarity::Epic => hud.epic,
            Rarity::Legendary => hud.legendary,
        }
    }
}

/// Where a skill stands: its level, its XP, and the XP that bounds the level.
///
/// The XP curve itself lives in the adventure core; the HUD only draws the
/// numbers it is handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XpProgress {
    /// The current level, `1..=MAX_LEVEL`.
    pub level: u8,
    /// Total XP earned in the skill.
    pub xp: u64,
    /// The total XP at which [`Self::level`] began.
    pub level_xp: u64,
    /// The total XP at which the next level begins. Ignored at
    /// [`MAX_LEVEL`].
    pub next_level_xp: u64,
    /// XP earned this month.
    pub month_xp: u64,
    /// Bonus XP banked by a streak, drawn past the earned XP as a lighter
    /// overlay. Zero for none.
    pub rested_xp: u64,
}

impl XpProgress {
    /// Whether the skill has reached [`MAX_LEVEL`] (or has no next level).
    pub fn is_max(&self) -> bool {
        self.level >= MAX_LEVEL || self.next_level_xp <= self.level_xp
    }

    /// How far through the current level the skill is, `0..=1`.
    pub fn fraction(&self) -> f32 {
        if self.is_max() {
            return 1.0;
        }
        let span = (self.next_level_xp - self.level_xp) as f64;
        let into = self.xp.saturating_sub(self.level_xp) as f64;
        (into / span).clamp(0.0, 1.0) as f32
    }

    /// The share of the level the rested bonus covers past
    /// [`Self::fraction`], capped at the end of the level.
    pub fn rested_fraction(&self) -> f32 {
        if self.is_max() || self.rested_xp == 0 {
            return 0.0;
        }
        let span = (self.next_level_xp - self.level_xp) as f64;
        let rested = (self.rested_xp as f64 / span) as f32;
        rested.min(1.0 - self.fraction()).max(0.0)
    }

    /// XP still needed to reach the next level. Zero at [`MAX_LEVEL`].
    pub fn remaining(&self) -> u64 {
        if self.is_max() {
            return 0;
        }
        self.next_level_xp.saturating_sub(self.xp)
    }
}

/// Formats a count with thousands separators (`1,234,567`) without
/// allocating, so it can go straight into a `format_args!`.
#[derive(Debug, Clone, Copy)]
pub struct Grouped(pub u64);

impl fmt::Display for Grouped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        // The largest power of 1000 at or below `n`, then each group of three
        // digits below it.
        let mut scale = 1;
        while n / scale >= 1000 {
            scale *= 1000;
        }
        write!(f, "{}", n / scale)?;
        while scale > 1 {
            scale /= 1000;
            write!(f, ",{:03}", (n / scale) % 1000)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_puts_a_comma_every_three_digits() {
        assert_eq!(Grouped(0).to_string(), "0");
        assert_eq!(Grouped(999).to_string(), "999");
        assert_eq!(Grouped(1_000).to_string(), "1,000");
        assert_eq!(Grouped(13_034_431).to_string(), "13,034,431");
        assert_eq!(Grouped(1_000_007).to_string(), "1,000,007");
        assert_eq!(Grouped(u64::MAX).to_string(), "18,446,744,073,709,551,615");
    }

    #[test]
    fn rarity_climbs_one_tier_per_threshold_reached() {
        let t = [10.0, 100.0, 1_000.0, 10_000.0];
        assert_eq!(Rarity::tier(9.9, &t), Rarity::Common);
        assert_eq!(Rarity::tier(10.0, &t), Rarity::Uncommon);
        assert_eq!(Rarity::tier(999.0, &t), Rarity::Rare);
        assert_eq!(Rarity::tier(1_000.0, &t), Rarity::Epic);
        assert_eq!(Rarity::tier(1e9, &t), Rarity::Legendary);
    }

    fn progress(xp: u64, rested_xp: u64) -> XpProgress {
        XpProgress {
            level: 10,
            xp,
            level_xp: 1_000,
            next_level_xp: 2_000,
            month_xp: 0,
            rested_xp,
        }
    }

    #[test]
    fn fraction_measures_progress_through_the_level() {
        assert_eq!(progress(1_000, 0).fraction(), 0.0);
        assert_eq!(progress(1_250, 0).fraction(), 0.25);
        assert_eq!(progress(1_250, 0).remaining(), 750);
    }

    #[test]
    fn rested_bonus_stops_at_the_end_of_the_level() {
        assert_eq!(progress(1_250, 250).rested_fraction(), 0.25);
        assert_eq!(progress(1_750, 900).rested_fraction(), 0.25);
        assert_eq!(progress(1_250, 0).rested_fraction(), 0.0);
    }

    #[test]
    fn max_level_is_a_full_bar_with_nothing_remaining() {
        let max = XpProgress {
            level: MAX_LEVEL,
            xp: 13_034_431,
            level_xp: 13_034_431,
            next_level_xp: 13_034_431,
            month_xp: 0,
            rested_xp: 500,
        };
        assert!(max.is_max());
        assert_eq!(max.fraction(), 1.0);
        assert_eq!(max.rested_fraction(), 0.0);
        assert_eq!(max.remaining(), 0);
    }
}
