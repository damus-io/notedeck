//! oot_bitset – “Ocarina-style” compact bit-flags, ported from C
//!
//! Each flag is a 16-bit ID where
//!   • upper 12 bits → word index
//!   • lower  4 bits → bit  index inside that word
//!
//! The slice you pass in **must** be an array of `u16` words large enough
//! to hold the highest word index you will touch.

/// Extract the `word` part of an encoded flag (upper 12 bits).
#[inline(always)]
pub const fn bitset_index(flag: u16) -> usize {
    (flag >> 4) as usize
}

/// Convert an encoded flag to a one-bit mask (lower 4 bits → 0–15).
#[inline(always)]
pub const fn bitset_mask(flag: u16) -> u16 {
    1u16 << (flag & 0x0F)
}

/// Test whether a given flag is set.
///
/// # Panics
/// Panics if `set` is not long enough to contain the word referenced by `flag`.
#[inline(always)]
pub fn bitset_get(set: &[u16], flag: u16) -> bool {
    let idx = bitset_index(flag);
    (set[idx] & bitset_mask(flag)) != 0
}

/// Set (enable) a flag.
///
/// # Panics
/// Panics if `set` is not long enough.
#[inline(always)]
pub fn bitset_set(set: &mut [u16], flag: u16) {
    let idx = bitset_index(flag);
    set[idx] |= bitset_mask(flag);
}

/// Clear (disable) a flag.
///
/// # Panics
/// Panics if `set` is not long enough.
#[inline(always)]
pub fn bitset_clear(set: &mut [u16], flag: u16) {
    let idx = bitset_index(flag);
    set[idx] &= !bitset_mask(flag);
}

/// Borrow the underlying 16-bit word for direct fiddling, mirroring the C macro.
///
/// Useful if you need to write several flags in the same word in one go.
///
/// # Panics
/// Panics if `set` is not long enough.
#[inline(always)]
pub fn bitset_word_mut(set: &mut [u16], flag: u16) -> &mut u16 {
    let idx = bitset_index(flag);
    &mut set[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(non_camel_case_types)]
    #[repr(u16)]
    pub enum Flag {
        HAS_SEEN_BOB = 0x10, // word 0 bit 0
    }

    #[test]
    fn roundtrip() {
        let mut save_flags = [0u16; 2];

        // Set a couple
        bitset_set(&mut save_flags, Flag::HAS_SEEN_BOB as u16);
        assert!(bitset_get(&save_flags, Flag::HAS_SEEN_BOB as u16));

        // Clear one and re-check
        bitset_clear(&mut save_flags, Flag::HAS_SEEN_BOB as u16);
        assert!(!bitset_get(&save_flags, Flag::HAS_SEEN_BOB as u16));
    }
}
