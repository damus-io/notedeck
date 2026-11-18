//! Data tools that edit agent specs, so `nd agentium spec list | show | set
//! | rm` changes a spec's model, prompt, toolsets or policy without a
//! rebuild.
//!
//! A spec is looked up record-first: the account's kind-31993 record of a
//! slug, else the built-in of that slug (see
//! [`agentium_core::agent_spec::record`]). `set` writes the account's
//! record, starting from whatever the slug resolves to now, so
//! `spec set butler --model qwen3` overrides the built-in butler with one
//! field changed. `rm` publishes a tombstone, after which the slug falls
//! back to its built-in again. Reads answer from nostrdb alone; writes
//! publish PNS-wrapped through an embedded engine, as the profile tools do
//! ([`super::profile`]).

use std::collections::BTreeMap;

use agentium_core::agent_spec::record::{
    self, build_agent_spec_delete_event_at, build_agent_spec_event_at,
    load_agent_spec_records_from_ndb, SpecRecord, SpecSource,
};
use agentium_core::agent_spec::{builtin, AgentSpec};
use agentium_core::cli::config::replacing;
use agentium_core::cli::profile::render_published;
use agentium_core::cli::term::{now_secs, paint, relative_time, SGR_BOLD};
use agentium_core::config::validate_slug;
use agentium_core::session_events::BuiltEvent;
use notedeck::{AppTool, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize};

use super::control::{event_hex, Writer};
use super::{open_txn, TOOL_CATEGORY};

/// Every spec tool, ready to register.
pub(super) fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(SpecList),
        RegisteredTool::new(SpecShow),
        RegisteredTool::new(SpecSet),
        RegisteredTool::new(SpecRm),
    ]
}

/// The selected account's spec records, by slug.
fn records(cx: &ToolContext) -> Result<BTreeMap<String, SpecRecord>, String> {
    let txn = open_txn(cx)?;
    let author = *cx.accounts.selected_account_pubkey();
    Ok(load_agent_spec_records_from_ndb(cx.ndb, &txn, &author))
}

/// The writer's spec records, read through its own engine's db.
fn writer_records(writer: &Writer) -> Result<BTreeMap<String, SpecRecord>, String> {
    let txn = writer.txn()?;
    Ok(load_agent_spec_records_from_ndb(
        writer.engine.ndb(),
        &txn,
        &writer.author,
    ))
}

/// `slug`, checked as a spec slug.
fn checked_slug(slug: &str) -> Result<&str, String> {
    validate_slug(slug).map_err(|e| e.to_string())?;
    Ok(slug)
}

/// The slug argument every tool on one spec takes.
fn slug_arg() -> ToolArg {
    ToolArg::new(
        "slug",
        ToolArgType::String,
        "The spec's slug, e.g. `butler`: 1-63 of a-z, 0-9 and -.",
    )
    .required(true)
    .positional()
}

/// One spec as the tools answer it: where it comes from and what it is.
#[derive(Serialize)]
struct SpecJson {
    slug: String,
    /// `account` (the account's record) or `builtin`.
    source: SpecSource,
    /// Whether an account record replaces a built-in of the same slug.
    overrides_builtin: bool,
    /// The record's `created_at`; `null` for a built-in.
    updated_at: Option<u64>,
    /// The spec, as `--file` takes it; `null` when the record can't be used.
    spec: Option<AgentSpec>,
    /// Why the account's record can't be used; `null` when it can.
    error: Option<String>,
}

impl SpecJson {
    /// `slug` as `records` resolve it; `None` when nothing has the slug.
    fn resolve(slug: &str, records: &BTreeMap<String, SpecRecord>) -> Option<Self> {
        let source = record::source(slug, records)?;
        let record = records.get(slug);
        let (spec, error) = match record::lookup(slug, records) {
            Ok(spec) => (Some(spec), None),
            Err(err) => (None, Some(err.to_string())),
        };
        Some(Self {
            slug: slug.to_owned(),
            source,
            overrides_builtin: record.is_some() && builtin::SLUGS.contains(&slug),
            updated_at: record.map(|r| r.updated_at),
            spec,
            error,
        })
    }

    /// Where it comes from, in words.
    fn origin(&self, now: u64) -> String {
        match (self.source, self.updated_at) {
            (SpecSource::Builtin, _) => "built-in".to_owned(),
            (SpecSource::Account, Some(at)) if self.overrides_builtin => {
                format!(
                    "account, overrides the built-in, {}",
                    relative_time(now, at)
                )
            }
            (SpecSource::Account, at) => {
                format!("account, {}", relative_time(now, at.unwrap_or_default()))
            }
        }
    }

    /// Its header line: the slug, its model and toolsets, and its origin.
    fn header(&self, now: u64, color: bool) -> String {
        let mut out = paint(color, SGR_BOLD, &self.slug);
        match &self.spec {
            Some(spec) => {
                let toolsets: Vec<String> = spec
                    .toolsets
                    .iter()
                    .map(|t| format!("{:?}", t.kind()).to_lowercase())
                    .collect();
                out.push_str(&format!(
                    "  {}/{}  [{}]",
                    spec.model.provider,
                    spec.model.model,
                    toolsets.join(", ")
                ));
            }
            None => out.push_str("  (unusable)"),
        }
        out.push_str(&format!("  ({})\n", self.origin(now)));
        if let Some(error) = &self.error {
            out.push_str(&format!("  {error}\n"));
        }
        out
    }
}

/// Arguments for the tools that take none.
#[derive(Deserialize)]
struct NoArgs {}

/// `agentium_spec_list`: every spec this account can run.
struct SpecList;

/// [`SpecList`]'s answer, in slug order.
#[derive(Serialize)]
#[serde(transparent)]
struct Specs(Vec<SpecJson>);

impl AppTool for SpecList {
    type Args = NoArgs;
    type Output = Specs;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_spec_list",
            "List the agent specs a session can run under (`nd agentium spawn --spec <slug>`): the built-ins and the account's own records, a record replacing the built-in of its slug. `[{slug, source, overrides_builtin, updated_at, spec, error}]`, `source` being `account` or `builtin`.",
            vec![],
        )
        .titled("List agent specs")
        .read_only()
        .command(&["agentium", "spec", "list"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, _args: Self::Args) -> Result<Self::Output, String> {
        let records = records(cx)?;
        Ok(Specs(
            record::slugs(&records)
                .into_iter()
                .filter_map(|slug| SpecJson::resolve(slug, &records))
                .collect(),
        ))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let now = now_secs();
        Some(out.0.iter().map(|spec| spec.header(now, true)).collect())
    }
}

/// `agentium_spec_show`: one spec in full.
struct SpecShow;

/// Arguments for the tools acting on one spec.
#[derive(Deserialize)]
struct SlugArgs {
    slug: String,
}

impl AppTool for SpecShow {
    type Args = SlugArgs;
    type Output = SpecJson;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_spec_show",
            "Show one agent spec in full, as the account's record or the built-in has it: `{slug, source, overrides_builtin, updated_at, spec, error}`. `spec` is the JSON `nd agentium spec set --file` takes.",
            vec![slug_arg()],
        )
        .titled("Show agent spec")
        .read_only()
        .command(&["agentium", "spec", "show"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        SpecJson::resolve(slug, &records(cx)?).ok_or_else(|| no_such_spec(slug))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let mut text = out.header(now_secs(), true);
        if let Some(spec) = &out.spec {
            let json = serde_json::to_string_pretty(spec).unwrap_or_default();
            text.push_str(&json);
            text.push('\n');
        }
        Some(text)
    }
}

/// The error for a slug neither the account nor the built-ins have.
fn no_such_spec(slug: &str) -> String {
    format!(
        "no agent spec {slug:?}; the built-in specs are {} — create one with \
         `nd agentium spec set {slug} --file spec.json`",
        builtin::SLUGS.join(", ")
    )
}

/// `agentium_spec_set`: write the account's record of a spec.
struct SpecSet;

/// Arguments for [`SpecSet`].
#[derive(Deserialize)]
struct SetArgs {
    slug: String,
    #[serde(default)]
    json: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

/// What `set` and `rm` published.
#[derive(Serialize)]
struct Published {
    /// `created`, `updated` or `removed`.
    action: &'static str,
    event_id: String,
    slug: String,
}

impl AppTool for SpecSet {
    type Args = SetArgs;
    type Output = Published;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_spec_set",
            "Write the account's record of an agent spec: a whole-spec replace, validated first, that every host of the account runs from its next turn. Starts from `json` when given, else from the spec the slug has now (the account's record, else the built-in), then applies `provider`, `model` and `prompt`. A record of a built-in's slug replaces that built-in until `agentium_spec_rm`. Answers `{action, event_id, slug}`, `action` being `created` or `updated`.",
            vec![
                slug_arg(),
                ToolArg::new("json", ToolArgType::String, "The whole spec as JSON, the shape `agentium_spec_show` answers under `spec`; its `slug` must match. On a command line give it with --file <path>.")
                    .from_file("file"),
                ToolArg::new("provider", ToolArgType::String, "The provider slug its model is served by: an OpenAI backend profile of the account's, or `openai`, `anthropic`, `ollama`."),
                ToolArg::new("model", ToolArgType::String, "The model id, as the provider knows it."),
                ToolArg::new("prompt", ToolArgType::String, "The system prompt. On a command line give it with --prompt-file <path> (or - for stdin).")
                    .from_file("prompt-file"),
            ],
        )
        .titled("Set agent spec")
        .command(&["agentium", "spec", "set"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?.to_owned();
        let writer = Writer::new(cx)?;
        let records = writer_records(&writer)?;
        let previous = records.get(&slug).map(|r| r.updated_at);
        let spec = edited_spec(&slug, &records, args)?;
        let built = build_agent_spec_event_at(&spec, previous.map(replacing), &writer.secret)
            .map_err(|e| format!("agent spec {slug}: {e}"))?;
        Ok(Published {
            action: if previous.is_some() {
                "updated"
            } else {
                "created"
            },
            event_id: publish(&writer, &built, &slug)?,
            slug,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let what = format!("{} agent spec {}", out.action, out.slug);
        Some(render_published(&what, &out.event_id))
    }
}

/// The spec `set` writes: `args.json`, else the slug's current spec, with
/// the field edits applied. Not validated here: building the record does.
fn edited_spec(
    slug: &str,
    records: &BTreeMap<String, SpecRecord>,
    args: SetArgs,
) -> Result<AgentSpec, String> {
    let has_edits = args.provider.is_some() || args.model.is_some() || args.prompt.is_some();
    let mut spec = match &args.json {
        Some(json) => {
            let spec: AgentSpec =
                serde_json::from_str(json).map_err(|e| format!("the spec doesn't parse: {e}"))?;
            if spec.slug != slug {
                return Err(format!("the spec's slug is {:?}, not {slug:?}", spec.slug));
            }
            spec
        }
        None if !has_edits => {
            return Err(format!(
                "nothing to set: give --file, --provider, --model or --prompt-file for {slug}"
            ))
        }
        None => record::lookup(slug, records).map_err(|err| match err {
            record::LookupError::Unknown(_) => no_such_spec(slug),
            err => format!("{err}; replace it whole with --file"),
        })?,
    };
    if let Some(provider) = args.provider {
        spec.model.provider = provider;
    }
    if let Some(model) = args.model {
        spec.model.model = model;
    }
    if let Some(prompt) = args.prompt {
        spec.prompt = prompt;
    }
    Ok(spec)
}

/// Publish a built kind-31993 event as the writer, answering its id in hex.
fn publish(writer: &Writer, built: &BuiltEvent, slug: &str) -> Result<String, String> {
    writer
        .engine
        .publish_event(built)
        .map_err(|e| format!("publishing agent spec {slug}: {e:?}"))?;
    Ok(event_hex(built))
}

/// `agentium_spec_rm`: remove the account's record of a spec.
struct SpecRm;

impl AppTool for SpecRm {
    type Args = SlugArgs;
    type Output = Published;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_spec_rm",
            "Remove the account's record of an agent spec (a tombstone revision). A built-in of the same slug is what the slug runs again. Answers `{action, event_id, slug}`.",
            vec![slug_arg()],
        )
        .titled("Remove agent spec")
        .command(&["agentium", "spec", "rm"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        let writer = Writer::new(cx)?;
        let Some(previous) = writer_records(&writer)?.get(slug).map(|r| r.updated_at) else {
            return Err(match builtin::SLUGS.contains(&slug) {
                true => format!("{slug} is built-in and the account has no record of it to remove"),
                false => no_such_spec(slug),
            });
        };
        let built =
            build_agent_spec_delete_event_at(slug, Some(replacing(previous)), &writer.secret)
                .map_err(|e| format!("building the agent spec delete: {e}"))?;
        Ok(Published {
            action: "removed",
            event_id: publish(&writer, &built, slug)?,
            slug: slug.to_owned(),
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let what = format!("removed agent spec {}", out.slug);
        Some(render_published(&what, &out.event_id))
    }
}
