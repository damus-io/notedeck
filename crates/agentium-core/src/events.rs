//! Events: what a spec's triggers bind to.
//!
//! Anything can emit an [`Event`]: an app in-process (headway's due cards),
//! or a script outside notedeck over the nd socket. Dave knows no kind of
//! event; it matches each one against the account's specs'
//! [`Trigger`](crate::agent_spec::Trigger)s by its [`source`](Event::source)
//! and [`meta`](Event::meta), and a trigger that matches starts a session.
//!
//! ```text
//! headway/card_due ─┐
//! nostr/dm         ─┼─▶ Event { source, key, content, meta } ─▶ Trigger::matches
//! ext/<anything>   ─┘
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The source prefix of events pushed from outside notedeck, over the nd
/// socket: `ext/<name>`.
pub const EXTERNAL_APP: &str = "ext";

/// One occurrence from a source.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Who emitted it, as `<app>/<name>` (see [`is_source_name`]), e.g.
    /// `headway/card_due`.
    pub source: String,
    /// Names this occurrence within its source, so the same one seen twice
    /// (by two hosts, or after a restart) is one firing.
    pub key: String,
    /// The occurrence's text, if it has any: a message's body. A trigger's
    /// prompt reads it as `{content}`.
    #[serde(default)]
    pub content: String,
    /// What the source says about it, by key. Values are lists so a card's
    /// labels match `label=butler` whatever else it carries. A trigger's
    /// prompt reads each as `{<key>}`.
    #[serde(default)]
    pub meta: BTreeMap<String, Vec<String>>,
}

/// Whether `name` is a source name: `<app>/<name>`, two non-empty parts of
/// lowercase ASCII letters, digits, `_` and `-`.
pub fn is_source_name(name: &str) -> bool {
    let Some((app, event)) = name.split_once('/') else {
        return false;
    };
    is_source_part(app) && is_source_part(event)
}

/// One half of a source name.
fn is_source_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source name is exactly one app and one name, both slug-like.
    #[test]
    fn source_names_are_app_slash_name() {
        for good in ["headway/card_due", "ext/cron-daily", "nostr/dm"] {
            assert!(is_source_name(good), "{good}");
        }
        for bad in [
            "",
            "headway",
            "/card_due",
            "headway/",
            "headway/card/due",
            "Headway/card_due",
            "headway/card due",
        ] {
            assert!(!is_source_name(bad), "{bad}");
        }
    }

    /// An event reads from JSON with only its source and key, and refuses a
    /// field it doesn't know.
    #[test]
    fn an_event_reads_from_json_strictly() {
        let event: Event = serde_json::from_str(r#"{"source":"ext/ping","key":"1"}"#).unwrap();
        assert_eq!(event.content, "");
        assert!(event.meta.is_empty());
        assert!(
            serde_json::from_str::<Event>(r#"{"source":"ext/ping","key":"1","at":0}"#).is_err()
        );
    }
}
