//! The OSRS experience curve: how much XP each level from 1 to 99 takes.
//!
//! Level `l` needs `⌊Σ_{i=1}^{l−1} ⌊i + 300·2^(i/7)⌋ / 4⌋` XP, so level 2 is
//! 83 XP, level 50 is 101,333, and level 99 is 13,034,431, about half of it
//! earned past level 92. XP past 99's keeps counting, but the level stops
//! there.

/// The highest level a skill reaches.
pub const MAX_LEVEL: u8 = 99;

/// The `i`th term of the curve's sum: what going from level `i` to `i + 1`
/// adds, before the sum is quartered.
fn term(i: u32) -> u64 {
    (f64::from(i) + 300.0 * 2f64.powf(f64::from(i) / 7.0)).floor() as u64
}

/// The XP level `level` starts at: 0 for level 1 (and for 0), and the
/// curve's value at [`MAX_LEVEL`] for anything above it.
pub fn xp_for_level(level: u8) -> u64 {
    let level = level.min(MAX_LEVEL);
    let sum: u64 = (1..u32::from(level)).map(term).sum();
    sum / 4
}

/// The level `xp` reaches: the highest one whose [`xp_for_level`] it has,
/// from 1 to [`MAX_LEVEL`].
pub fn level_for_xp(xp: u64) -> u8 {
    let mut sum = 0;
    for level in 1..MAX_LEVEL {
        sum += term(u32::from(level));
        if xp < sum / 4 {
            return level;
        }
    }
    MAX_LEVEL
}

/// The XP still needed to reach the level after the one `xp` is at, or
/// `None` at [`MAX_LEVEL`].
pub fn xp_to_next_level(xp: u64) -> Option<u64> {
    let level = level_for_xp(xp);
    (level < MAX_LEVEL).then(|| xp_for_level(level + 1) - xp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values the OSRS skill table prints.
    #[test]
    fn matches_the_table() {
        assert_eq!(xp_for_level(1), 0);
        assert_eq!(xp_for_level(2), 83);
        assert_eq!(xp_for_level(10), 1_154);
        assert_eq!(xp_for_level(50), 101_333);
        assert_eq!(xp_for_level(92), 6_517_253);
        assert_eq!(xp_for_level(99), 13_034_431);
        assert_eq!(xp_for_level(120), 13_034_431);
    }

    /// A level starts exactly at its XP, and one XP short is the level
    /// below.
    #[test]
    fn level_for_xp_inverts_the_curve() {
        assert_eq!(level_for_xp(0), 1);
        assert_eq!(level_for_xp(82), 1);
        assert_eq!(level_for_xp(83), 2);
        for level in 2..=MAX_LEVEL {
            let at = xp_for_level(level);
            assert_eq!(level_for_xp(at), level, "level {level}");
            assert_eq!(level_for_xp(at - 1), level - 1, "below level {level}");
        }
        assert_eq!(level_for_xp(200_000_000), MAX_LEVEL);
    }

    /// What is left to the next level, and nothing past the last.
    #[test]
    fn to_next_level() {
        assert_eq!(xp_to_next_level(0), Some(83));
        assert_eq!(xp_to_next_level(80), Some(3));
        assert_eq!(xp_to_next_level(13_034_431), None);
    }
}
