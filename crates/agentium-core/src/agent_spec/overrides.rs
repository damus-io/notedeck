//! What one session changes about the spec it runs under.
//!
//! A spec is a preset: `nd agentium spawn --spec butler` runs the butler's
//! model, prompt and budget. A spawn can change those for the session it
//! starts, and for no other, without writing the account's spec record:
//!
//! ```text
//! spawn --spec butler --model anthropic/claude-haiku-5-5 --max-steps 5
//!   kind-31989: ["agent-spec","butler"] ["model",".."] ["spec-max-steps","5"]
//!   kind-31988: the same tags, so a restart or another host sees them
//!
//! host, each turn:  record or built-in "butler"
//!                     └─ SpecOverrides::apply(model, ..) ─▶ resolved AgentSpec
//!                                                            └─ validate()
//! ```
//!
//! The model is not one of these fields: it rides the session's own `model`
//! tag, which a spawn's `--model` and a `/model` already set, so a spec
//! session's model and any other session's are one value. [`SpecOverrides::apply`]
//! takes it beside the rest.
//!
//! The overrides reach only the model, the prompt and the budget. Toolsets,
//! policy and trust stay the spec's, so an override can't add a tool or
//! widen what one may do; [`AgentSpec::validate`] still runs on the result,
//! so a zero step budget is refused here as it is in a record.
//!
//! Each field is its own tag, a decimal number for the budget, so a reader
//! takes the value straight from the note.

use serde::Serialize;

use super::{AgentSpec, ModelRef, SpecError};
use crate::session_events::get_tag_value;

/// The tag replacing the spec's system prompt.
pub const SPEC_PROMPT_TAG: &str = "spec-prompt";
/// The tag replacing the spec's [`Budget::max_steps`](super::Budget::max_steps).
pub const SPEC_MAX_STEPS_TAG: &str = "spec-max-steps";
/// The tag replacing the spec's
/// [`Budget::max_cost_cents`](super::Budget::max_cost_cents).
pub const SPEC_MAX_COST_CENTS_TAG: &str = "spec-max-cost-cents";
/// The tag replacing the spec's
/// [`Budget::timeout_secs`](super::Budget::timeout_secs).
pub const SPEC_TIMEOUT_SECS_TAG: &str = "spec-timeout-secs";

/// One session's changes to its spec's prompt and budget. Every field is
/// optional, and `None` keeps the spec's own value. The default changes
/// nothing.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SpecOverrides {
    /// Replaces the spec's system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Replaces the spec's step cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    /// Replaces the spec's spend cap, in US cents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_cents: Option<u32>,
    /// Replaces the spec's wall-clock limit, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl SpecOverrides {
    /// No overrides: the spec as it is.
    pub const NONE: SpecOverrides = SpecOverrides {
        prompt: None,
        max_steps: None,
        max_cost_cents: None,
        timeout_secs: None,
    };

    /// Whether it changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::NONE
    }

    /// The overrides a kind-31989 spawn command or kind-31988 state carries.
    /// A number that doesn't parse is dropped with a warning, keeping the
    /// spec's own value, the way an unknown permission mode keeps the
    /// host's default.
    pub fn from_note(note: &nostrdb::Note<'_>) -> Self {
        fn number<T: std::str::FromStr>(note: &nostrdb::Note<'_>, tag: &str) -> Option<T> {
            let raw = get_tag_value(note, tag)?;
            let parsed = raw.trim().parse().ok();
            if parsed.is_none() {
                tracing::warn!("ignoring {tag} {raw:?}: not a number");
            }
            parsed
        }
        Self {
            prompt: get_tag_value(note, SPEC_PROMPT_TAG)
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
            max_steps: number(note, SPEC_MAX_STEPS_TAG),
            max_cost_cents: number(note, SPEC_MAX_COST_CENTS_TAG),
            timeout_secs: number(note, SPEC_TIMEOUT_SECS_TAG),
        }
    }

    /// Write each set field as its own tag on `builder`; an empty set writes
    /// none, so a plain spec session's events are what they were.
    pub fn write_tags<'a>(
        &self,
        mut builder: nostrdb::NoteBuilder<'a>,
    ) -> nostrdb::NoteBuilder<'a> {
        if let Some(prompt) = self.prompt.as_deref().filter(|p| !p.is_empty()) {
            builder = builder.start_tag().tag_str(SPEC_PROMPT_TAG).tag_str(prompt);
        }
        let numbers = [
            (SPEC_MAX_STEPS_TAG, self.max_steps.map(u64::from)),
            (SPEC_MAX_COST_CENTS_TAG, self.max_cost_cents.map(u64::from)),
            (SPEC_TIMEOUT_SECS_TAG, self.timeout_secs),
        ];
        for (tag, value) in numbers {
            if let Some(value) = value {
                builder = builder.start_tag().tag_str(tag).tag_str(&value.to_string());
            }
        }
        builder
    }

    /// `base` with `model` and these overrides applied, checked again: what
    /// the session's turns run under. `model` is the session's own (its
    /// `model` tag), read by [`ModelRef::overridden`]; `is_provider` says
    /// whether a slug names a provider this host can resolve.
    pub fn apply(
        &self,
        base: &AgentSpec,
        model: Option<&str>,
        is_provider: impl Fn(&str) -> bool,
    ) -> Result<AgentSpec, SpecError> {
        let mut spec = base.clone();
        if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
            spec.model = base.model.overridden(model, is_provider);
        }
        if let Some(prompt) = self.prompt.as_deref().filter(|p| !p.is_empty()) {
            spec.prompt = prompt.to_owned();
        }
        if let Some(steps) = self.max_steps {
            spec.budget.max_steps = steps;
        }
        if let Some(cents) = self.max_cost_cents {
            spec.budget.max_cost_cents = Some(cents);
        }
        if let Some(secs) = self.timeout_secs {
            spec.budget.timeout_secs = Some(secs);
        }
        spec.validate()?;
        Ok(spec)
    }
}

impl ModelRef {
    /// This model with `raw` in its place: `provider/model` when the part
    /// before the first `/` names a provider (`is_provider`), else a model on
    /// this one's provider. A model id with a `/` of its own
    /// (`meta-llama/llama-4`) stays whole unless its prefix is a provider.
    pub fn overridden(&self, raw: &str, is_provider: impl Fn(&str) -> bool) -> ModelRef {
        if let Some((provider, model)) = raw.split_once('/') {
            if !model.is_empty() && is_provider(provider) {
                return ModelRef {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                };
            }
        }
        ModelRef {
            provider: self.provider.clone(),
            model: raw.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::builtin;

    fn known(slug: &str) -> bool {
        matches!(slug, "anthropic" | "openai" | "work")
    }

    /// A bare model stays on the spec's provider; `provider/model` moves it
    /// when the prefix is a provider, and stays whole when it isn't.
    #[test]
    fn a_model_override_names_its_provider_or_keeps_the_specs() {
        let base = ModelRef {
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
        };
        let m = |raw| base.overridden(raw, known);
        assert_eq!(m("claude-haiku-5-5").provider, "anthropic");
        assert_eq!(m("claude-haiku-5-5").model, "claude-haiku-5-5");
        assert_eq!(m("work/qwen3").provider, "work");
        assert_eq!(m("work/qwen3").model, "qwen3");
        let whole = m("meta-llama/llama-4");
        assert_eq!(whole.provider, "anthropic");
        assert_eq!(whole.model, "meta-llama/llama-4");
        assert_eq!(m("work/").model, "work/", "no model after the provider");
    }

    /// Each set field replaces the spec's, unset ones keep it, and the
    /// toolsets, policy and trust are the spec's untouched.
    #[test]
    fn apply_replaces_only_what_is_set() {
        let butler = builtin::find("butler").unwrap();
        let overrides = SpecOverrides {
            prompt: Some("Be terse.".into()),
            max_steps: Some(5),
            ..Default::default()
        };
        let resolved = overrides
            .apply(&butler, Some("openai/gpt-5.3"), known)
            .unwrap();
        assert_eq!(resolved.prompt, "Be terse.");
        assert_eq!(resolved.budget.max_steps, 5);
        assert_eq!(resolved.budget.max_cost_cents, butler.budget.max_cost_cents);
        assert_eq!(resolved.budget.timeout_secs, butler.budget.timeout_secs);
        assert_eq!(resolved.model.provider, "openai");
        assert_eq!(resolved.model.model, "gpt-5.3");
        assert_eq!(resolved.toolsets, butler.toolsets);
        assert_eq!(resolved.policy, butler.policy);
        assert_eq!(resolved.trust, butler.trust);

        let unchanged = SpecOverrides::default()
            .apply(&butler, None, known)
            .unwrap();
        assert_eq!(unchanged, butler);
    }

    /// The resolved spec is validated: a zero step cap is refused as it is
    /// in a record.
    #[test]
    fn apply_validates_the_result() {
        let butler = builtin::find("butler").unwrap();
        let zero = SpecOverrides {
            max_steps: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            zero.apply(&butler, None, known),
            Err(SpecError::ZeroSteps(_))
        ));
    }

    /// Overrides round-trip through a note's tags, and an empty set writes
    /// no tag at all.
    #[test]
    fn overrides_round_trip_through_tags() {
        let overrides = SpecOverrides {
            prompt: Some("Review only.".into()),
            max_steps: Some(7),
            max_cost_cents: Some(150),
            timeout_secs: Some(600),
        };
        let sk = [7u8; 32];
        let note = |o: &SpecOverrides| {
            o.write_tags(nostrdb::NoteBuilder::new().kind(31989).content(""))
                .sign(&sk)
                .build()
                .unwrap()
        };
        assert_eq!(SpecOverrides::from_note(&note(&overrides)), overrides);
        let empty = note(&SpecOverrides::default());
        assert_eq!(empty.tags().count(), 0);
        assert!(SpecOverrides::from_note(&empty).is_empty());
    }
}
