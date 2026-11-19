//! Agent specs as account records: one PNS-wrapped kind-31993 note per
//! spec, so a user changes a prompt, a model, a toolset or a policy without
//! a rebuild, and every host of the account sees the same spec.
//!
//! ```text
//! nd agentium spec set butler --model …  ─▶ kind-31993 ["d","butler"] … (PNS-wrapped)
//!                                                   │
//!           lookup("butler") ─▶ account record ─────┘  else builtin::find("butler")
//! ```
//!
//! A record is the same [`AgentSpec`] a built-in's JSON gives, kept in tags
//! (rule 20 of the repo's coding patterns) so nothing parses JSON on a read.
//! The prompt is prose a person wrote, so it is the note's `content`. Every
//! other field is a tag, in the spec's own order:
//!
//! | tag | from |
//! |---|---|
//! | `["d", slug]` | `slug` |
//! | `["provider", p]`, `["model", m]` | `model` |
//! | `["toolset", "notedeck", app, …]` | `Toolset::Notedeck` |
//! | `["toolset", "web"]` | `Toolset::Web` |
//! | `["toolset", "coding", "local_executor"]` | `Toolset::Coding` on the host |
//! | `["toolset", "coding", "microsandbox", git]` | `Toolset::Coding` in a guest |
//! | `["toolset", "sub_agent", spec]` | `Toolset::SubAgent` |
//! | `["widen", tool, to, why]` | `policy.widen` |
//! | `["tighten", "tool" \| "class", name, to]` | `policy.tighten` |
//! | `["max_steps", n]`, `["max_cost_cents", n]`, `["timeout_secs", n]` | `budget` |
//!
//! The reader is as strict as [`AgentSpec::from_json`]'s `deny_unknown_fields`:
//! a tag it doesn't know fails the whole record, so a restriction a newer
//! writer added is never silently dropped. A record that fails is not skipped
//! either: [`lookup`] answers its error rather than falling back to the
//! built-in of that slug, which the user meant to replace.
//!
//! A removal is a tombstone revision (`["deleted", "true"]`), after which the
//! slug falls back to its built-in, if it has one.

use std::collections::BTreeMap;

use nostrdb::{Ndb, Note, Transaction};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{builtin, AgentSpec, Budget, ModelRef, Policy, Runner, Tighten, Toolset, Widen};
use crate::config::validate_slug;
use crate::session_events::{
    finalize_built_event, get_tag_value, init_note_builder, BuiltEvent, EventBuildError,
};

/// Nostr kind of an agent spec record (parameterized replaceable, NIP-33):
/// one note per spec, d-tag = its slug. Account-wide, like backend profiles,
/// and only ever published PNS-wrapped: a prompt is private config.
pub const AI_AGENT_SPEC_KIND: u32 = 31993;

/// The newest revision of one account spec record.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecRecord {
    /// Its d-tag.
    pub slug: String,
    /// Its note's `created_at`, which the next revision must exceed.
    pub updated_at: u64,
    /// The spec, or why the record can't be used.
    pub spec: Result<AgentSpec, String>,
}

/// Where a looked-up spec came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecSource {
    /// The account's own record.
    Account,
    /// Shipped with notedeck ([`builtin`]).
    Builtin,
}

/// Why [`lookup`] has no spec to run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    /// Neither the account nor the built-ins have the slug.
    #[error("no agent spec {0:?}; the built-in specs are {list}", list = builtin::SLUGS.join(", "))]
    Unknown(String),
    /// The account's record of the slug doesn't read as a spec.
    #[error("agent spec {slug:?}'s account record can't be used: {why}")]
    BadRecord { slug: String, why: String },
}

/// The spec named `slug`: the account's record when it has one, else the
/// built-in of that slug. A record that doesn't read is an error, never a
/// quiet fall back to the built-in it was written to replace.
pub fn lookup(
    slug: &str,
    records: &BTreeMap<String, SpecRecord>,
) -> Result<AgentSpec, LookupError> {
    if let Some(record) = records.get(slug) {
        return record.spec.clone().map_err(|why| LookupError::BadRecord {
            slug: slug.to_owned(),
            why,
        });
    }
    builtin::find(slug).ok_or_else(|| LookupError::Unknown(slug.to_owned()))
}

/// Where [`lookup`] finds `slug`, if anywhere.
pub fn source(slug: &str, records: &BTreeMap<String, SpecRecord>) -> Option<SpecSource> {
    if records.contains_key(slug) {
        return Some(SpecSource::Account);
    }
    builtin::SLUGS
        .contains(&slug)
        .then_some(SpecSource::Builtin)
}

/// Every slug [`lookup`] knows, in order: the built-ins and the account's
/// records together, each once.
pub fn slugs(records: &BTreeMap<String, SpecRecord>) -> std::collections::BTreeSet<&str> {
    builtin::SLUGS
        .iter()
        .copied()
        .chain(records.keys().map(String::as_str))
        .collect()
}

/// Build a kind-31993 record of `spec`, validated first: the next revision
/// of its slug. `created_at` `None` is now; a replacing writer passes one
/// past the previous revision's (see `cli::config::replacing`). The inner
/// note holds the prompt in the clear, so publish it PNS-wrapped only.
pub fn build_agent_spec_event_at(
    spec: &AgentSpec,
    created_at: Option<u64>,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    spec.validate()
        .map_err(|e| EventBuildError::Build(e.to_string()))?;
    let mut b = init_note_builder(AI_AGENT_SPEC_KIND, &spec.prompt, created_at);
    b = b.start_tag().tag_str("d").tag_str(&spec.slug);
    b = b
        .start_tag()
        .tag_str("provider")
        .tag_str(&spec.model.provider);
    b = b.start_tag().tag_str("model").tag_str(&spec.model.model);
    for toolset in &spec.toolsets {
        b = b.start_tag().tag_str("toolset");
        b = match toolset {
            Toolset::Notedeck { apps } => {
                let mut b = b.tag_str("notedeck");
                for app in apps {
                    b = b.tag_str(app);
                }
                b
            }
            Toolset::Web => b.tag_str("web"),
            Toolset::Coding {
                runner: Runner::LocalExecutor,
            } => b.tag_str("coding").tag_str("local_executor"),
            Toolset::Coding {
                runner: Runner::Microsandbox { git },
            } => b
                .tag_str("coding")
                .tag_str("microsandbox")
                .tag_str(&enum_str(git)),
            Toolset::SubAgent { spec } => b.tag_str("sub_agent").tag_str(spec),
        };
    }
    for widen in &spec.policy.widen {
        b = b
            .start_tag()
            .tag_str("widen")
            .tag_str(&widen.tool)
            .tag_str(&enum_str(&widen.to))
            .tag_str(&widen.why);
    }
    for tighten in &spec.policy.tighten {
        b = b.start_tag().tag_str("tighten");
        b = match (&tighten.tool, &tighten.class) {
            (Some(tool), _) => b.tag_str("tool").tag_str(tool),
            (None, Some(class)) => b.tag_str("class").tag_str(&enum_str(class)),
            // validate() refused a tighten naming neither.
            (None, None) => b,
        };
        b = b.tag_str(&enum_str(&tighten.to));
    }
    let budget = &spec.budget;
    b = b
        .start_tag()
        .tag_str("max_steps")
        .tag_str(&budget.max_steps.to_string());
    if let Some(cents) = budget.max_cost_cents {
        b = b
            .start_tag()
            .tag_str("max_cost_cents")
            .tag_str(&cents.to_string());
    }
    if let Some(secs) = budget.timeout_secs {
        b = b
            .start_tag()
            .tag_str("timeout_secs")
            .tag_str(&secs.to_string());
    }
    finalize_built_event(b, secret_key, AI_AGENT_SPEC_KIND)
}

/// Build a tombstone for the record `slug`: the same d-tag with a `deleted`
/// tag, which replaces the live record, so the slug falls back to its
/// built-in.
pub fn build_agent_spec_delete_event_at(
    slug: &str,
    created_at: Option<u64>,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    validate_slug(slug).map_err(|e| EventBuildError::Build(e.to_string()))?;
    let mut b = init_note_builder(AI_AGENT_SPEC_KIND, "", created_at);
    b = b.start_tag().tag_str("d").tag_str(slug);
    b = b.start_tag().tag_str("deleted").tag_str("true");
    finalize_built_event(b, secret_key, AI_AGENT_SPEC_KIND)
}

/// Whether a kind-31993 note is a tombstone.
pub fn is_agent_spec_deleted(note: &Note) -> bool {
    get_tag_value(note, "deleted").is_some()
}

/// Read a kind-31993 note back into the record [`build_agent_spec_event_at`]
/// built it from. `None` for a tombstone or a note with no valid slug, which
/// names no record; a record whose spec doesn't read or validate carries
/// why.
pub fn agent_spec_record_from_note(note: &Note) -> Option<SpecRecord> {
    if is_agent_spec_deleted(note) {
        return None;
    }
    let slug = get_tag_value(note, "d")?;
    validate_slug(slug).ok()?;
    Some(SpecRecord {
        slug: slug.to_owned(),
        updated_at: note.created_at(),
        spec: spec_from_note(note, slug)
            .and_then(|spec| spec.validate().map(|()| spec).map_err(|e| e.to_string())),
    })
}

/// The spec a record's tags and content spell, before validation.
fn spec_from_note(note: &Note, slug: &str) -> Result<AgentSpec, String> {
    let mut model = ModelRef {
        provider: String::new(),
        model: String::new(),
    };
    let mut toolsets = Vec::new();
    let mut policy = Policy::default();
    let mut max_steps = None;
    let mut max_cost_cents = None;
    let mut timeout_secs = None;
    for tag in note.tags() {
        let name = tag.get_str(0).ok_or("a tag with no readable name")?;
        let at = |i: u16| -> Result<&str, String> {
            tag.get_str(i)
                .ok_or_else(|| format!("its {name} tag has no readable value {i}"))
        };
        match name {
            "d" => {}
            "provider" => model.provider = at(1)?.to_owned(),
            "model" => model.model = at(1)?.to_owned(),
            "toolset" => toolsets.push(toolset_from_tag(&tag)?),
            "widen" => policy.widen.push(Widen {
                tool: at(1)?.to_owned(),
                to: parse_enum(at(2)?)?,
                why: at(3)?.to_owned(),
            }),
            "tighten" => {
                let target = at(2)?;
                let (tool, class) = match at(1)? {
                    "tool" => (Some(target.to_owned()), None),
                    "class" => (None, Some(parse_enum(target)?)),
                    other => return Err(format!("a tighten of an unknown kind {other:?}")),
                };
                policy.tighten.push(Tighten {
                    tool,
                    class,
                    to: parse_enum(at(3)?)?,
                });
            }
            "max_steps" => max_steps = Some(parse_number(name, at(1)?)?),
            "max_cost_cents" => max_cost_cents = Some(parse_number(name, at(1)?)?),
            "timeout_secs" => timeout_secs = Some(parse_number(name, at(1)?)?),
            other => return Err(format!("an unknown tag {other:?}")),
        }
    }
    Ok(AgentSpec {
        slug: slug.to_owned(),
        model,
        prompt: note.content().to_owned(),
        toolsets,
        policy,
        budget: Budget {
            max_steps: max_steps.ok_or("no max_steps tag")?,
            max_cost_cents,
            timeout_secs,
        },
    })
}

/// One `toolset` tag back into its [`Toolset`].
fn toolset_from_tag(tag: &nostrdb::Tag) -> Result<Toolset, String> {
    let kind = tag.get_str(1).ok_or("a toolset tag with no kind")?;
    let arg = |i: u16| {
        tag.get_str(i)
            .ok_or_else(|| format!("a {kind} toolset tag with no value {i}"))
    };
    let toolset = match kind {
        "notedeck" => Toolset::Notedeck {
            apps: (2..tag.count())
                .map(|i| arg(i).map(str::to_owned))
                .collect::<Result<_, _>>()?,
        },
        "web" => Toolset::Web,
        "coding" => Toolset::Coding {
            runner: match arg(2)? {
                "local_executor" => Runner::LocalExecutor,
                "microsandbox" => Runner::Microsandbox {
                    git: parse_enum(arg(3)?)?,
                },
                other => return Err(format!("a coding toolset on an unknown runner {other:?}")),
            },
        },
        "sub_agent" => Toolset::SubAgent {
            spec: arg(2)?.to_owned(),
        },
        other => return Err(format!("an unknown toolset {other:?}")),
    };
    Ok(toolset)
}

/// A unit enum's serde spelling (`gated`, `mutate_workspace`, `read_only`):
/// the same words a spec's JSON uses.
fn enum_str<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        // Only unit enums come through here, which serialize as strings.
        _ => String::new(),
    }
}

/// [`enum_str`] read back.
fn parse_enum<T: DeserializeOwned>(word: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(word.to_owned()))
        .map_err(|_| format!("an unknown value {word:?}"))
}

/// A budget tag's number.
fn parse_number<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("its {name} tag {value:?} isn't a number"))
}

/// The account's spec records by slug, each slug's newest revision; a slug
/// whose newest revision is a tombstone is absent. See
/// [`load_account_records`](crate::session_loader::load_account_records).
pub fn load_agent_spec_records_from_ndb(
    ndb: &Ndb,
    txn: &Transaction,
    author: &nostrdb_net::Pubkey,
) -> BTreeMap<String, SpecRecord> {
    crate::session_loader::load_account_records(
        ndb,
        txn,
        author,
        AI_AGENT_SPEC_KIND,
        agent_spec_record_from_note,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::{Approval, SandboxGit};
    use crate::test_support::{pns_ingest, pns_ndb, pubkey_of, TempNdb, TEST_SECKEY};
    use crate::tool::Capability;

    fn load(db: &TempNdb) -> BTreeMap<String, SpecRecord> {
        let txn = Transaction::new(&db.ndb).unwrap();
        load_agent_spec_records_from_ndb(&db.ndb, &txn, &pubkey_of(&TEST_SECKEY))
    }

    /// A spec using every field the tags carry: each toolset kind but one
    /// (Coding and SubAgent can't share with Notedeck's private data), a
    /// widen, both tighten forms, and a full budget.
    fn every_field() -> Vec<AgentSpec> {
        let researcher = builtin::find("researcher").unwrap();
        let mut butler = builtin::find("butler").unwrap();
        butler.slug = "concierge".into();
        butler.policy.tighten.push(Tighten {
            tool: None,
            class: Some(Capability::ActAsUser),
            to: Approval::Impossible,
        });
        butler.budget.timeout_secs = Some(600);
        butler.budget.max_cost_cents = Some(250);
        let reviewer = AgentSpec::from_json(include_str!("examples/reviewer.json")).unwrap();
        let mut sandboxed = AgentSpec::from_json(include_str!("examples/coder.json")).unwrap();
        sandboxed.toolsets = vec![Toolset::Coding {
            runner: Runner::Microsandbox {
                git: SandboxGit::ReadWrite,
            },
        }];
        vec![researcher, butler, reviewer, sandboxed]
    }

    /// Build → PNS wrap → ingest → load gives back each spec as it was, and
    /// the envelope carries none of the prompt.
    #[tokio::test]
    async fn specs_round_trip_through_pns() {
        let db = pns_ndb();
        for (i, spec) in every_field().into_iter().enumerate() {
            let built =
                build_agent_spec_event_at(&spec, Some(1000 + i as u64), &TEST_SECKEY).unwrap();
            let wrapped = pns_ingest(&db.ndb, &built).await;
            let first_line = spec.prompt.lines().next().unwrap_or_default();
            if !first_line.is_empty() {
                assert!(
                    !wrapped.contains(first_line),
                    "the envelope leaks the prompt"
                );
            }
            let loaded = load(&db);
            let record = &loaded[&spec.slug];
            assert_eq!(record.updated_at, 1000 + i as u64);
            assert_eq!(record.spec.as_ref(), Ok(&spec), "{}", spec.slug);
        }
    }

    /// The account's record of a built-in's slug is what lookup answers; its
    /// tombstone brings the built-in back; a slug only the account has is
    /// found, and one nobody has is unknown.
    #[tokio::test]
    async fn a_record_overrides_the_builtin_until_removed() {
        let db = pns_ndb();
        let mut butler = builtin::find("butler").unwrap();
        butler.model.model = "qwen3".into();
        let mut scout = builtin::find("researcher").unwrap();
        scout.slug = "scout".into();
        for built in [
            build_agent_spec_event_at(&butler, Some(2000), &TEST_SECKEY),
            build_agent_spec_event_at(&scout, Some(2000), &TEST_SECKEY),
        ] {
            pns_ingest(&db.ndb, &built.unwrap()).await;
        }
        let records = load(&db);
        assert_eq!(lookup("butler", &records).unwrap().model.model, "qwen3");
        assert_eq!(source("butler", &records), Some(SpecSource::Account));
        assert_eq!(lookup("scout", &records).unwrap(), scout);
        assert_eq!(
            slugs(&records).into_iter().collect::<Vec<_>>(),
            ["butler", "researcher", "scout"]
        );
        assert!(matches!(
            lookup("nobody", &records),
            Err(LookupError::Unknown(_))
        ));

        let tombstone = build_agent_spec_delete_event_at("butler", Some(2001), &TEST_SECKEY);
        pns_ingest(&db.ndb, &tombstone.unwrap()).await;
        let records = load(&db);
        assert_eq!(
            lookup("butler", &records).unwrap(),
            builtin::find("butler").unwrap()
        );
        assert_eq!(source("butler", &records), Some(SpecSource::Builtin));
    }

    /// A record with a tag this build doesn't know fails as a whole and
    /// shadows the built-in, so a restriction it can't read is never
    /// dropped by running the built-in instead.
    #[tokio::test]
    async fn an_unreadable_record_fails_rather_than_falling_back() {
        let db = pns_ndb();
        let mut b = init_note_builder(AI_AGENT_SPEC_KIND, "prompt", Some(3000));
        for (name, value) in [
            ("d", "butler"),
            ("provider", "anthropic"),
            ("model", "claude-sonnet-5-5"),
            ("max_steps", "5"),
            ("deny", "web_fetch"),
        ] {
            b = b.start_tag().tag_str(name).tag_str(value);
        }
        b = b.start_tag().tag_str("toolset").tag_str("web");
        let built = finalize_built_event(b, &TEST_SECKEY, AI_AGENT_SPEC_KIND).unwrap();
        pns_ingest(&db.ndb, &built).await;

        let records = load(&db);
        let err = lookup("butler", &records).unwrap_err();
        assert!(matches!(err, LookupError::BadRecord { .. }), "{err}");
        assert!(err.to_string().contains("\"deny\""), "{err}");
    }

    /// The writer refuses a spec that doesn't validate, so a record that
    /// can't run is never published.
    #[test]
    fn an_invalid_spec_is_never_built() {
        let mut spec = builtin::find("butler").unwrap();
        spec.toolsets.push(Toolset::Web);
        let err = build_agent_spec_event_at(&spec, None, &TEST_SECKEY).unwrap_err();
        assert!(err.to_string().contains("network"), "{err}");
    }
}
