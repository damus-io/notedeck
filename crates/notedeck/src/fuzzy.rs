//! A small fuzzy matcher for filtering lists as someone types: the command
//! bar, pickers, quick switchers.
//!
//! A query matches a candidate when its characters appear in the candidate in
//! order, not necessarily next to each other (`ob` matches **o**pen **b**oard),
//! ignoring case and the query's spaces. The [score](fuzzy_match) ranks the
//! matches a person means first: a match at the very start, then matches on
//! word starts, then runs of adjacent characters, and scattered ones last.
//!
//! [`fuzzy_match`] is pure and allocates nothing of its own: the matched
//! positions go into a buffer the caller owns and reuses, so filtering a list
//! once per keystroke costs no allocation once the buffer has grown.

/// What every matched character is worth.
const MATCH: i32 = 16;

/// Extra for a match on the candidate's first character, so a prefix match
/// outranks the same match further in.
const PREFIX: i32 = 8;

/// Extra for a match on a word's first character (see [`is_word_start`]).
const WORD_START: i32 = 10;

/// Extra for a match right after the previous one, so `card` ranks
/// `Move card` over `c-a-r-d`: more than a word start saves, so a run of
/// adjacent letters beats the same letters each starting a word.
const ADJACENT: i32 = 16;

/// The most one gap between matched characters can cost, one point per
/// skipped character, so one long gap can't sink an otherwise good match.
const MAX_GAP_PENALTY: i32 = 8;

/// Match `query` against `candidate`, returning its score, or `None` when the
/// query's characters don't all appear in the candidate in order. Higher
/// scores are better matches; scores only compare between candidates matched
/// against the same query. An empty (or all-space) query matches everything
/// with a score of 0.
///
/// `positions` is cleared and filled with the byte offset in `candidate` of
/// each matched character, ascending, for highlighting them; it is left empty
/// when nothing matched. Case is ignored and whitespace in the query is
/// skipped, so `new card` matches `New card` and `NewCard` alike.
///
/// Each query character takes its first occurrence after the last match,
/// unless that occurrence starts no word and doesn't follow on from the last
/// match, and a later one that starts a word still leaves room for the rest of
/// the query: `ob` takes the `b` of `board` in `Open keyboard board`. Pure;
/// allocates nothing once `positions` has room for the query.
pub fn fuzzy_match(query: &str, candidate: &str, positions: &mut Vec<usize>) -> Option<i32> {
    positions.clear();
    let mut score = 0;
    // Where the next search starts: just past the previous match.
    let mut from = 0;
    for (qi, qc) in query.char_indices() {
        if qc.is_whitespace() {
            continue;
        }
        let rest = &query[qi + qc.len_utf8()..];
        let Some(mut at) = find_char(candidate, from, qc) else {
            positions.clear();
            return None;
        };
        let adjacent = !positions.is_empty() && at == from;
        if !adjacent && !is_word_start(candidate, at) {
            if let Some(better) = later_word_start(candidate, at, qc, rest) {
                at = better;
            }
        }

        score += MATCH;
        if at == 0 {
            score += PREFIX;
        }
        if is_word_start(candidate, at) {
            score += WORD_START;
        }
        if !positions.is_empty() && at == from {
            score += ADJACENT;
        } else {
            let skipped = candidate[from..at].chars().count() as i32;
            score -= skipped.min(MAX_GAP_PENALTY);
        }

        positions.push(at);
        from = at + char_len_at(candidate, at);
    }
    Some(score)
}

/// Whether `a` and `b` are the same letter, ignoring case.
fn same_char(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// The byte offset of the first character at or after `from` in `haystack`
/// that is `needle`, ignoring case.
fn find_char(haystack: &str, from: usize, needle: char) -> Option<usize> {
    haystack[from..]
        .char_indices()
        .find(|&(_, c)| same_char(c, needle))
        .map(|(i, _)| from + i)
}

/// A later occurrence of `needle` than the one at `at` that starts a word and
/// still leaves `rest` of the query to match after it.
fn later_word_start(haystack: &str, at: usize, needle: char, rest: &str) -> Option<usize> {
    let mut from = at + char_len_at(haystack, at);
    while let Some(next) = find_char(haystack, from, needle) {
        let after = next + char_len_at(haystack, next);
        if is_word_start(haystack, next) && is_subsequence(rest, &haystack[after..]) {
            return Some(next);
        }
        from = after;
    }
    None
}

/// Whether `query`'s non-space characters appear in `haystack` in order,
/// ignoring case.
fn is_subsequence(query: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|qc| hay.any(|c| same_char(c, qc)))
}

/// Whether the character at byte `at` starts a word: it opens the string,
/// follows something that isn't a letter or digit (a space, `_`, `-`, `/`),
/// or is a capital after a lowercase letter (`newCard`).
fn is_word_start(s: &str, at: usize) -> bool {
    let Some(prev) = s[..at].chars().next_back() else {
        return true;
    };
    if !prev.is_alphanumeric() {
        return true;
    }
    let cur = s[at..].chars().next().unwrap_or(' ');
    prev.is_lowercase() && cur.is_uppercase()
}

/// The length in bytes of the character at byte `at`.
fn char_len_at(s: &str, at: usize) -> usize {
    s[at..].chars().next().map_or(0, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The score of `query` against `candidate`, if it matches.
    fn score(query: &str, candidate: &str) -> Option<i32> {
        fuzzy_match(query, candidate, &mut Vec::new())
    }

    /// The matched positions of `query` in `candidate`, as the characters.
    fn matched(query: &str, candidate: &str) -> String {
        let mut positions = Vec::new();
        fuzzy_match(query, candidate, &mut positions);
        positions
            .iter()
            .map(|&at| candidate[at..].chars().next().unwrap())
            .collect()
    }

    /// `candidates` ordered best match for `query` first, dropping misses;
    /// ties keep their order.
    fn ranked<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        let mut scored: Vec<_> = candidates
            .iter()
            .filter_map(|c| score(query, c).map(|s| (s, *c)))
            .collect();
        scored.sort_by_key(|&(s, _)| std::cmp::Reverse(s));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn letters_in_order_match_and_others_dont() {
        assert!(score("nc", "New card").is_some());
        assert!(score("cn", "New card").is_none());
        assert!(score("x", "New card").is_none());
        assert!(score("new cards", "New card").is_none());
    }

    #[test]
    fn case_and_query_spaces_are_ignored() {
        assert_eq!(score("NEW CARD", "new card"), score("newcard", "New Card"));
        assert!(score("ÉT", "été").is_some());
    }

    #[test]
    fn an_empty_query_matches_everything_equally() {
        let mut positions = vec![3];
        assert_eq!(fuzzy_match("", "anything", &mut positions), Some(0));
        assert!(positions.is_empty());
        assert_eq!(score("  ", "anything"), Some(0));
    }

    #[test]
    fn a_miss_leaves_no_positions() {
        let mut positions = Vec::new();
        assert_eq!(fuzzy_match("nz", "New card", &mut positions), None);
        assert!(positions.is_empty());
    }

    #[test]
    fn positions_are_byte_offsets_of_the_matched_characters() {
        let mut positions = Vec::new();
        fuzzy_match("éc", "été card", &mut positions);
        assert_eq!(positions, vec![0, 6]);
    }

    #[test]
    fn a_prefix_outranks_a_word_start_outranks_mid_word() {
        assert_eq!(
            ranked("bo", &["keyboard", "Toggle board", "Boards"]),
            vec!["Boards", "Toggle board", "keyboard"]
        );
    }

    #[test]
    fn word_starts_outrank_scattered_letters() {
        assert_eq!(
            ranked("nc", &["announce", "New card"]),
            vec!["New card", "announce"]
        );
    }

    #[test]
    fn camel_case_and_separators_start_words() {
        assert_eq!(matched("nc", "newCard"), "nC");
        assert_eq!(matched("hc", "headway_cursor"), "hc");
        assert_eq!(
            ranked("hc", &["hatch", "headway_cursor"])[0],
            "headway_cursor"
        );
    }

    #[test]
    fn adjacent_letters_outrank_spread_ones() {
        assert_eq!(
            ranked("card", &["c-a-r-d", "Move card"]),
            vec!["Move card", "c-a-r-d"]
        );
    }

    #[test]
    fn a_later_word_start_is_preferred_when_the_rest_still_fits() {
        // The `b` of "keyboard" comes first, but "board" starts a word.
        assert_eq!(matched("ob", "Open keyboard board"), "Ob");
        let mut positions = Vec::new();
        fuzzy_match("ob", "Open keyboard board", &mut positions);
        assert_eq!(positions, vec![0, 14]);
    }

    #[test]
    fn a_later_word_start_is_skipped_when_the_rest_would_not_fit() {
        // Jumping to the second word's `b` would leave no `x` after it.
        let mut positions = Vec::new();
        fuzzy_match("abx", "abx b", &mut positions);
        assert_eq!(positions, vec![0, 1, 2]);
    }

    #[test]
    fn an_exact_match_beats_a_longer_one_with_the_same_start_only_by_ties() {
        // Same prefix, same adjacency: the matcher ties them; callers keep
        // their own order for ties.
        assert_eq!(score("board", "Board"), score("board", "Boards"));
    }

    #[test]
    fn a_reused_buffer_does_not_grow_past_the_query() {
        let mut positions = Vec::with_capacity(4);
        for candidate in ["New card", "Move card", "Open board", "nc"] {
            fuzzy_match("nc", candidate, &mut positions);
        }
        assert_eq!(positions.capacity(), 4);
    }
}
