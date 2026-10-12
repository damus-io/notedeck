//! Splitting a card title into the parts a narrow surface shows separately.
//!
//! Card titles are often full sentences shaped `CODE: headline — detail`, e.g.
//! `UT7c: the key list, bindings, Escape menu, settings and console drawn from
//! the atlas — crews-keys-2, bindings-1, …`. Cut to sidebar width that reads as
//! nothing useful, so every surface that shows a title in little room (a session
//! list row, a scene node, a hover card, a chat header) splits it the same way
//! here: the code goes in its own column or node, the headline is what's read,
//! and the detail is kept for hover.

/// Longest prefix, in chars, that still reads as a code (`UT7c`, `Crag 1.6`).
const CODE_MAX_CHARS: usize = 10;

/// Most whitespace-separated words a code may have.
const CODE_MAX_WORDS: usize = 2;

/// What separates the code from the headline.
const CODE_SEP: &str = ": ";

/// What separates the headline from the detail: an em dash with spaces.
const DETAIL_SEP: &str = " — ";

/// A card title split into `CODE: headline — detail`, as slices borrowed from
/// the title (see the module docs). Built by [`TitleParts::split`], which never
/// allocates, so a per-frame caller can split without caching the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleParts<'a> {
    /// The short code before the first `": "`, when that prefix is short enough
    /// to be one (at most 10 chars and 2 words of letters, digits, `.` and `-`).
    pub code: Option<&'a str>,
    /// What's left after the code, up to the first `" — "`, trimmed. The whole
    /// title when it has neither a code nor a dash.
    pub headline: &'a str,
    /// Everything after the first `" — "`, trimmed; `None` when there's no dash
    /// or nothing follows it.
    pub detail: Option<&'a str>,
}

impl<'a> TitleParts<'a> {
    /// Split `title` into its code, headline and detail.
    pub fn split(title: &'a str) -> Self {
        let title = title.trim();
        let (code, rest) = match title.split_once(CODE_SEP) {
            Some((prefix, rest)) if is_code(prefix) => (Some(prefix), rest),
            _ => (None, title),
        };
        // The title was trimmed, so a dash with nothing after it has lost
        // its trailing space; strip it rather than show it in the headline.
        let (headline, detail) = match rest.split_once(DETAIL_SEP) {
            Some((headline, detail)) => (headline, Some(detail.trim())),
            None => (
                rest.strip_suffix(DETAIL_SEP.trim_end()).unwrap_or(rest),
                None,
            ),
        };
        Self {
            code,
            headline: headline.trim(),
            detail: detail.filter(|d| !d.is_empty()),
        }
    }
}

/// Whether a title's pre-colon `prefix` is short and plain enough to be a code.
fn is_code(prefix: &str) -> bool {
    !prefix.trim().is_empty()
        && prefix.trim() == prefix
        && prefix.chars().count() <= CODE_MAX_CHARS
        && prefix.split_whitespace().count() <= CODE_MAX_WORDS
        && prefix
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | ' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(
        code: Option<&'a str>,
        headline: &'a str,
        detail: Option<&'a str>,
    ) -> TitleParts<'a> {
        TitleParts {
            code,
            headline,
            detail,
        }
    }

    #[test]
    fn code_headline_and_detail() {
        assert_eq!(
            TitleParts::split(
                "UT7c: the key list, bindings, Escape menu, settings and console drawn \
                 from the atlas — crews-keys-2, bindings-1, …"
            ),
            parts(
                Some("UT7c"),
                "the key list, bindings, Escape menu, settings and console drawn from the atlas",
                Some("crews-keys-2, bindings-1, …"),
            )
        );
    }

    #[test]
    fn two_word_code_with_a_dot() {
        assert_eq!(
            TitleParts::split("Crag 1.6: ledges you can mantle — climb-3"),
            parts(Some("Crag 1.6"), "ledges you can mantle", Some("climb-3"))
        );
    }

    #[test]
    fn crate_name_prefix_is_a_code() {
        assert_eq!(
            TitleParts::split("headway: one card-title split into code / headline / detail"),
            parts(
                Some("headway"),
                "one card-title split into code / headline / detail",
                None
            )
        );
    }

    #[test]
    fn long_prefix_is_no_code() {
        let title = "a morphing prop's foot worked out once: cached per frame — props-2";
        assert_eq!(
            TitleParts::split(title),
            parts(
                None,
                "a morphing prop's foot worked out once: cached per frame",
                Some("props-2")
            )
        );
    }

    #[test]
    fn three_words_or_punctuation_is_no_code() {
        assert_eq!(TitleParts::split("a b c: x").code, None);
        assert_eq!(TitleParts::split("[epic] dave: scene map").code, None);
        assert_eq!(TitleParts::split("QA2: x").code, Some("QA2"));
    }

    #[test]
    fn no_colon_no_dash_is_all_headline() {
        assert_eq!(
            TitleParts::split("  fix the sidebar  "),
            parts(None, "fix the sidebar", None)
        );
    }

    #[test]
    fn colon_without_space_is_not_a_separator() {
        assert_eq!(
            TitleParts::split("see 10:30 standup"),
            parts(None, "see 10:30 standup", None)
        );
    }

    #[test]
    fn plain_hyphen_is_not_the_detail_dash() {
        assert_eq!(
            TitleParts::split("QA2: re-run the - suite"),
            parts(Some("QA2"), "re-run the - suite", None)
        );
    }

    #[test]
    fn empty_detail_is_none() {
        assert_eq!(
            TitleParts::split("QA2: headline — "),
            parts(Some("QA2"), "headline", None)
        );
    }
}
