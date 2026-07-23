//! Small shared helpers for the session engine.
//!
//! Ported from `notedeck`'s `abbrev.rs`/string helpers: this crate is
//! deliberately free of any `notedeck` dependency, so it carries its own
//! copies of the few char-boundary utilities the session protocol needs.

/// Snap `index` (a byte offset) down to the nearest UTF-8 char boundary at or
/// below it, so slicing `&s[..floor_char_boundary(s, index)]` never splits a
/// codepoint. Mirrors `notedeck::abbrev::floor_char_boundary`.
#[inline]
pub(crate) fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else {
        let lower_bound = index.saturating_sub(3);
        let new_index = s.as_bytes()[lower_bound..=index]
            .iter()
            .rposition(|b| is_utf8_char_boundary(*b));

        // SAFETY: a char boundary is always within four bytes below `index`.
        unsafe { lower_bound + new_index.unwrap_unchecked() }
    }
}

#[inline]
fn is_utf8_char_boundary(c: u8) -> bool {
    // Bit magic equivalent to: b < 128 || b >= 192
    (c as i8) >= -0x40
}

/// Truncate a string to at most `max_chars` characters, appending an ellipsis
/// when truncation occurs. Counts by `char` so multi-byte codepoints stay
/// intact.
///
/// Uses `char_indices().nth()` rather than counting every char up front, so a
/// long string is only walked as far as the cut point. `notedeck`'s
/// `abbrev.rs` has byte-boundary helpers along these lines, but this crate is
/// deliberately free of any `notedeck` dependency, so it carries its own.
pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        // Fewer than or exactly `max_chars` chars: return unchanged.
        None => s.to_string(),
        // `byte_idx` is where the first `max_chars` chars end.
        Some((byte_idx, _)) => format!("{}...", &s[..byte_idx]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn long_string_truncated() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn multibyte_boundary_preserved() {
        // Each 'é' is two bytes; truncation must land on a char boundary.
        assert_eq!(truncate("éééé", 2), "éé...");
    }
}
