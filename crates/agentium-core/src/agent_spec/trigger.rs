//! When a spec runs on its own: a spec's [`Trigger`]s.
//!
//! A trigger is config, like the rest of the spec, so a scheduled agent is a
//! spec with a `triggers` list rather than code on the host. The butler's
//! weekly idea review is one:
//!
//! ```json
//! "triggers": [{
//!   "kind": "card_due",
//!   "label": "butler",
//!   "prompt": "Scheduled review of {card}: its review date, {due}, has arrived. …"
//! }]
//! ```
//!
//! A host running Dave watches the account's boards and spawns a session
//! under the spec, once per firing, with the trigger's prompt as its first
//! message (`notedeck_dave`'s `spec_triggers` module).

use serde::{Deserialize, Serialize};

/// The placeholder in a [`Trigger::CardDue`] prompt for the card's
/// `headway:<board>/<word-id>` ref.
pub const CARD_PLACEHOLDER: &str = "{card}";

/// The placeholder in a [`Trigger::CardDue`] prompt for the due date, as
/// `YYYY-MM-DD`.
pub const DUE_PLACEHOLDER: &str = "{due}";

/// A condition that starts a session under the spec, unasked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// The due date of a headway card carrying `label` has arrived: run once
    /// per card per due date, on the card, outside terminal columns. The
    /// spec moves the date on to schedule its next run.
    CardDue {
        /// The label that opts a card in, e.g. `butler`.
        label: String,
        /// The session's first message, with [`CARD_PLACEHOLDER`] and
        /// [`DUE_PLACEHOLDER`] filled in by [`Trigger::card_due_prompt`].
        prompt: String,
    },
}

impl Trigger {
    /// Whether the trigger names everything it needs: a label and a prompt.
    pub fn is_complete(&self) -> bool {
        match self {
            Trigger::CardDue { label, prompt } => {
                !label.trim().is_empty() && !prompt.trim().is_empty()
            }
        }
    }

    /// A [`Trigger::CardDue`] prompt for the card `card_ref`, due `due`.
    pub fn card_due_prompt(prompt: &str, card_ref: &str, due: &str) -> String {
        prompt
            .replace(CARD_PLACEHOLDER, card_ref)
            .replace(DUE_PLACEHOLDER, due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both placeholders are filled, wherever and however often they occur.
    #[test]
    fn a_card_due_prompt_fills_its_placeholders() {
        let prompt = Trigger::card_due_prompt(
            "Review {card}, due {due}. Then {card} again.",
            "headway:ideas/a-b-c",
            "2026-11-18",
        );
        assert_eq!(
            prompt,
            "Review headway:ideas/a-b-c, due 2026-11-18. Then headway:ideas/a-b-c again."
        );
    }

    /// A trigger reads from JSON by its kind, and refuses a field it doesn't
    /// know rather than dropping it.
    #[test]
    fn a_trigger_reads_from_json_strictly() {
        let trigger: Trigger =
            serde_json::from_str(r#"{"kind":"card_due","label":"butler","prompt":"Go {card}"}"#)
                .unwrap();
        assert!(trigger.is_complete());
        assert!(serde_json::from_str::<Trigger>(
            r#"{"kind":"card_due","label":"x","prompt":"y","every":"week"}"#
        )
        .is_err());
        let blank = Trigger::CardDue {
            label: " ".into(),
            prompt: "y".into(),
        };
        assert!(!blank.is_complete());
    }
}
