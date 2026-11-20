//! When a spec runs on its own: a spec's [`Trigger`]s.
//!
//! A trigger is config, like the rest of the spec, so a scheduled agent is a
//! spec with a `triggers` list rather than code on the host. It binds the
//! spec to [`Event`]s by their source: Dave knows no kind of trigger, only
//! events and the bindings that match them. The butler's weekly idea review
//! is one:
//!
//! ```json
//! "triggers": [{
//!   "source": "headway/card_due",
//!   "match": {"label": ["butler"]},
//!   "session": "new",
//!   "prompt": "Scheduled review of {card}: its review date, {due}, has arrived. …"
//! }]
//! ```
//!
//! A host running Dave matches the events it sees against the account's
//! specs and spawns a session under each spec a trigger of which matches,
//! once per event, with the trigger's prompt as its first message
//! (`notedeck_dave`'s `spec_triggers` module).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::events::{is_source_name, Event};

/// The placeholder in a trigger's prompt for the event's
/// [`content`](Event::content). Any other `{<key>}` reads the event's meta
/// value(s) of that key.
pub const CONTENT_PLACEHOLDER: &str = "content";

/// A binding from events to a session under the spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    /// The [`Event::source`] it binds to, e.g. `headway/card_due`.
    pub source: String,
    /// Which of the source's events: each key's values must all be among
    /// the event's meta values of that key. Empty matches every event.
    #[serde(rename = "match", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub r#match: BTreeMap<String, Vec<String>>,
    /// Which session an event runs in.
    #[serde(default)]
    pub session: TriggerSession,
    /// The session's first message, its placeholders filled by
    /// [`Trigger::prompt_for`].
    pub prompt: String,
}

/// Which session a trigger's event runs in.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerSession {
    /// A new session per event (`"new"`).
    #[default]
    New,
    /// One session per value of this meta key (`{"per": "<key>"}`): a
    /// conversation's events feed the session it started.
    Per(String),
}

/// Why a [`Trigger`] can't run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TriggerError {
    /// Its source isn't `<app>/<name>`.
    #[error("a trigger on {0:?}, which isn't an <app>/<name> source")]
    Source(String),
    /// Its prompt is blank.
    #[error("a trigger on {0} with no prompt")]
    NoPrompt(String),
    /// A match key with no values, which would match every event.
    #[error("a trigger on {on} matching {key:?} against no values")]
    EmptyMatch { on: String, key: String },
    /// `session: {per: ""}`.
    #[error("a trigger on {0} with a per-conversation session naming no key")]
    NoPerKey(String),
}

impl Trigger {
    /// Check the trigger can run: a source name, a prompt, every match key
    /// given a value, and a `per` session naming its key.
    pub fn validate(&self) -> Result<(), TriggerError> {
        if !is_source_name(&self.source) {
            return Err(TriggerError::Source(self.source.clone()));
        }
        if self.prompt.trim().is_empty() {
            return Err(TriggerError::NoPrompt(self.source.clone()));
        }
        if let Some(key) = self
            .r#match
            .iter()
            .find_map(|(k, v)| v.is_empty().then_some(k))
        {
            return Err(TriggerError::EmptyMatch {
                on: self.source.clone(),
                key: key.clone(),
            });
        }
        if let TriggerSession::Per(key) = &self.session {
            if key.trim().is_empty() {
                return Err(TriggerError::NoPerKey(self.source.clone()));
            }
        }
        Ok(())
    }

    /// Whether `event` fires this trigger: the same source, and each match
    /// key's values all among the event's meta values of that key.
    pub fn matches(&self, event: &Event) -> bool {
        event.source == self.source
            && self.r#match.iter().all(|(key, wanted)| {
                let Some(have) = event.meta.get(key) else {
                    return false;
                };
                wanted.iter().all(|w| have.contains(w))
            })
    }

    /// The session's first message for `event`: the prompt with
    /// `{content}` and each `{<meta key>}` filled (several values joined by
    /// `", "`). A placeholder the event has no value for is left as written.
    pub fn prompt_for(&self, event: &Event) -> String {
        let mut out = String::with_capacity(self.prompt.len());
        let mut rest = self.prompt.as_str();
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after
                .find(['{', '}'])
                .filter(|&i| after.as_bytes()[i] == b'}')
            else {
                // No closing brace before the next opening one: a literal.
                out.push('{');
                rest = after;
                continue;
            };
            let key = &after[..close];
            if key == CONTENT_PLACEHOLDER {
                out.push_str(&event.content);
            } else if let Some(values) = event.meta.get(key) {
                out.push_str(&values.join(", "));
            } else {
                out.push_str(&rest[open..open + close + 2]);
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `headway/card_due` event for a card with `labels`.
    fn due(labels: &[&str]) -> Event {
        let mut meta = BTreeMap::new();
        meta.insert("card".into(), vec!["headway:ideas/a-b-c".into()]);
        meta.insert("due".into(), vec!["2026-11-18".into()]);
        meta.insert(
            "label".into(),
            labels.iter().map(|l| l.to_string()).collect(),
        );
        Event {
            source: "headway/card_due".into(),
            key: "a-b-c:2026-11-18".into(),
            content: "idea one".into(),
            meta,
        }
    }

    fn trigger(json: &str) -> Trigger {
        let trigger: Trigger = serde_json::from_str(json).unwrap();
        trigger.validate().unwrap();
        trigger
    }

    /// A match needs the same source and every wanted value among the
    /// event's values of that key, whatever else the event carries.
    #[test]
    fn a_trigger_matches_by_source_and_every_wanted_value() {
        let butler = trigger(
            r#"{"source":"headway/card_due","match":{"label":["butler"]},"prompt":"Go {card}"}"#,
        );
        assert!(butler.matches(&due(&["butler"])));
        assert!(butler.matches(&due(&["idea", "butler"])));
        assert!(!butler.matches(&due(&["idea"])));

        let both = trigger(
            r#"{"source":"headway/card_due","match":{"label":["butler","urgent"]},"prompt":"Go"}"#,
        );
        assert!(both.matches(&due(&["urgent", "butler", "idea"])));
        assert!(!both.matches(&due(&["butler"])));

        let mut no_labels = due(&[]);
        no_labels.meta.remove("label");
        assert!(!butler.matches(&no_labels));

        let mut elsewhere = due(&["butler"]);
        elsewhere.source = "ext/card_due".into();
        assert!(!butler.matches(&elsewhere));

        let any = trigger(r#"{"source":"headway/card_due","prompt":"Go"}"#);
        assert!(any.matches(&no_labels));
    }

    /// `{content}` and meta keys are filled wherever and however often they
    /// occur; several values join with ", "; an unknown placeholder and a
    /// stray brace stay as written.
    #[test]
    fn a_prompt_fills_its_placeholders() {
        let t = trigger(
            r#"{"source":"headway/card_due","prompt":"Review {card} ({content}), due {due}. Labels: {label}. {nope} {card} {{due}} {"}"#,
        );
        assert_eq!(
            t.prompt_for(&due(&["butler", "idea"])),
            "Review headway:ideas/a-b-c (idea one), due 2026-11-18. Labels: butler, idea. \
             {nope} headway:ideas/a-b-c {2026-11-18} {"
        );
    }

    /// A trigger reads from JSON strictly: `session` defaults to new,
    /// `{"per": key}` reads, and an unknown field or session is refused.
    #[test]
    fn a_trigger_reads_from_json_strictly() {
        let t = trigger(r#"{"source":"nostr/dm","prompt":"Reply to {content}"}"#);
        assert_eq!(t.session, TriggerSession::New);
        let t = trigger(r#"{"source":"nostr/dm","session":{"per":"thread"},"prompt":"Reply"}"#);
        assert_eq!(t.session, TriggerSession::Per("thread".into()));
        let round: Trigger = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(round, t);
        for bad in [
            r#"{"source":"nostr/dm","prompt":"x","every":"week"}"#,
            r#"{"source":"nostr/dm","prompt":"x","session":"old"}"#,
            r#"{"kind":"card_due","label":"butler","prompt":"x"}"#,
        ] {
            assert!(serde_json::from_str::<Trigger>(bad).is_err(), "{bad}");
        }
    }

    /// validate() refuses a bad source, a blank prompt, a valueless match
    /// key and a `per` with no key.
    #[test]
    fn validate_refuses_what_cant_run() {
        let ok = trigger(r#"{"source":"headway/card_due","prompt":"Go"}"#);
        let check = |f: fn(&mut Trigger)| {
            let mut t = ok.clone();
            f(&mut t);
            t.validate().unwrap_err()
        };
        assert!(matches!(
            check(|t| t.source = "card_due".into()),
            TriggerError::Source(_)
        ));
        assert!(matches!(
            check(|t| t.prompt = " ".into()),
            TriggerError::NoPrompt(_)
        ));
        assert!(matches!(
            check(|t| {
                t.r#match.insert("label".into(), vec![]);
            }),
            TriggerError::EmptyMatch { .. }
        ));
        assert!(matches!(
            check(|t| t.session = TriggerSession::Per(" ".into())),
            TriggerError::NoPerKey(_)
        ));
    }
}
