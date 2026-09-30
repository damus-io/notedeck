//! Open an entity by a reference string, from any app.
//!
//! An [`OpenUri`] names *what* to open — any reference a registered
//! [`ReferenceParser`](crate::ReferenceParser) recognizes, such as
//! `agentium:some-word-id` or `headway:board/word-word-word` — plus optional
//! parameters after a `?`. An app raises it as
//! [`AppAction::Open`](crate::AppAction::Open) without knowing which app owns the
//! scheme; the shell resolves the reference through the registered parsers and
//! opens the resulting note exactly as a click on its inline chip would.
//!
//! ```text
//! agentium:some-word-id?msg="launch a /code-review for the work done in this session"
//! ```

/// A request to open the entity `reference` names, with optional parameters.
///
/// Built with [`OpenUri::parse`] from a `reference?key=value&…` string, or
/// directly. Only `msg` is understood today; unknown keys are ignored so a newer
/// sender doesn't break an older receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenUri {
    /// The reference to open, exactly as a [`ReferenceParser`](crate::ReferenceParser)
    /// recognizes it (e.g. `agentium:some-word-id`). Never empty.
    pub reference: String,
    /// A message to hand the opened entity, e.g. a prompt to send into an
    /// agentium session. The shell carries it through [`AppAction::Open`](crate::AppAction::Open)
    /// to the owning app: Dave sends it into the opened session as a user
    /// message; apps that take no message log and drop it.
    pub msg: Option<String>,
}

impl OpenUri {
    /// An open of `reference` with no parameters.
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            msg: None,
        }
    }

    /// Parse `reference?key=value&…`.
    ///
    /// Splits at the first `?`; the part before it (trimmed) is the reference and
    /// must be non-empty, or this returns `None`. The part after is a
    /// `application/x-www-form-urlencoded` query: pairs are percent-decoded (`+`
    /// is a space), one pair of surrounding double quotes is stripped from a
    /// value so `msg="two words"` works, `msg` is kept (the last one wins), and
    /// any other key is ignored. A `?` with nothing after it is the same as none.
    pub fn parse(uri: &str) -> Option<Self> {
        let (reference, query) = match uri.split_once('?') {
            Some((reference, query)) => (reference, Some(query)),
            None => (uri, None),
        };
        let reference = reference.trim();
        if reference.is_empty() {
            return None;
        }

        let mut open = Self::new(reference);
        let pairs = query.map(|q| url::form_urlencoded::parse(q.as_bytes()));
        for (key, value) in pairs.into_iter().flatten() {
            if key == "msg" {
                open.msg = Some(unquote(&value).to_owned());
            }
        }
        Some(open)
    }
}

/// `value` with one pair of surrounding double quotes removed, or `value`
/// unchanged when it isn't wrapped in them.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for the expected parse of `reference` with `msg`.
    fn open(reference: &str, msg: Option<&str>) -> Option<OpenUri> {
        Some(OpenUri {
            reference: reference.to_owned(),
            msg: msg.map(str::to_owned),
        })
    }

    #[test]
    fn parse_table() {
        let cases: &[(&str, Option<OpenUri>)] = &[
            // A bare reference, no parameters.
            ("agentium:some-word-id", open("agentium:some-word-id", None)),
            // Percent-decoded, and `+` is a space.
            (
                "agentium:a-b-c?msg=a%20b",
                open("agentium:a-b-c", Some("a b")),
            ),
            (
                "agentium:a-b-c?msg=a+b",
                open("agentium:a-b-c", Some("a b")),
            ),
            // One pair of surrounding quotes is stripped (jb55's pseudocode form).
            (
                "agentium:a-b-c?msg=\"launch a /code-review\"",
                open("agentium:a-b-c", Some("launch a /code-review")),
            ),
            // Quoted *and* encoded.
            (
                "agentium:a-b-c?msg=%22hi%20there%22",
                open("agentium:a-b-c", Some("hi there")),
            ),
            // A lone quote is not a pair.
            (
                "agentium:a-b-c?msg=\"hi",
                open("agentium:a-b-c", Some("\"hi")),
            ),
            // Unknown keys are ignored, in any position.
            (
                "headway:b/x-y-z?foo=1&msg=hi&bar",
                open("headway:b/x-y-z", Some("hi")),
            ),
            ("headway:b/x-y-z?foo=1", open("headway:b/x-y-z", None)),
            // An empty msg is still a msg.
            ("headway:b/x-y-z?msg=", open("headway:b/x-y-z", Some(""))),
            // A `?` with nothing after it is the same as none.
            ("headway:b/x-y-z?", open("headway:b/x-y-z", None)),
            // Only the first `?` splits; later ones belong to the query.
            (
                "agentium:a-b-c?msg=why?",
                open("agentium:a-b-c", Some("why?")),
            ),
            // The reference is trimmed.
            ("  agentium:a-b-c  ", open("agentium:a-b-c", None)),
            // No reference: nothing to open.
            ("", None),
            ("   ", None),
            ("?msg=hi", None),
        ];
        for (uri, want) in cases {
            assert_eq!(&OpenUri::parse(uri), want, "parsing {uri:?}");
        }
    }
}
