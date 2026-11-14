//! `headway_review` and `headway_diff`: the tools that read git, in the
//! caller's checkout. The socket is local, so the app reads the same
//! filesystem the caller does; `nd` sends its working directory as
//! `repo_dir` (a [`Path`](ToolArgType::Path) argument that defaults to it),
//! and the source files as canonical paths the app reads itself.
//!
//! Both share their git work with the `headway` CLI: [`headway::review`]
//! gathers a record, [`headway::diff`] reads one back.

use std::path::Path;

use headway::diff::{DiffBody, DiffOut, DiffPlan};
use headway::event::{ReviewFields, ReviewSource, ReviewSourceRole, resolve_card};
use headway::git::NoteKind;
use headway::review::{self, ReviewFlags, SourceFlag};
use headway::store::BoardAction;
use notedeck::{AppTool, SlowTool, ToolArg, ToolArgType, ToolContext, ToolJob, ToolSpec};
use serde::{Deserialize, Serialize, Serializer};

use super::{EditAck, Route, board_view, card_arg, card_board_arg, default_board_arg, mutate};

/// The variable a command line falls back to for `agentium`'s default, as
/// the `headway` CLI reads it: the caller's Dave session.
const AGENTIUM_ENV: &str = "AGENTIUM_SESSION";

/// A tool's `repo_dir`: the caller's checkout, which `nd` fills with its
/// working directory.
fn repo_dir_arg(description: &'static str) -> ToolArg {
    ToolArg::new("repo_dir", ToolArgType::Path, description).default_cwd()
}

/// The checkout a tool reads git in, or an error naming the argument: a
/// model calling the tool has no working directory to fall back to.
fn repo_dir(given: Option<String>) -> Result<String, String> {
    given.ok_or_else(|| {
        "repo_dir is required: the checkout to read git in (nd sends its working directory)"
            .to_owned()
    })
}

/// `headway_review`: record a commit on a card for review, gathered from
/// git in the caller's checkout.
pub(super) struct Review;

/// Arguments for [`Review`].
#[derive(Deserialize)]
pub(super) struct ReviewArgs {
    card: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    repo_dir: Option<String>,
    #[serde(default)]
    explainer: Option<String>,
    #[serde(default)]
    deploy: Option<String>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    agentium: Option<String>,
    #[serde(default)]
    default_agentium: Option<String>,
    #[serde(default)]
    explainer_source: Vec<String>,
    #[serde(default)]
    code_source: Vec<String>,
    #[serde(flatten)]
    route: Route,
}

/// What [`Review`] answers: the edit's ack, and what it recorded.
#[derive(Serialize)]
pub(super) struct ReviewAck {
    #[serde(flatten)]
    ack: EditAck,
    /// The record's fields, by wire tag name.
    review: serde_json::Map<String, serde_json::Value>,
    /// The source files stored on it.
    sources: Vec<SourceSummary>,
    /// [`review::recorded_text`] of the record, for the text answer.
    #[serde(skip)]
    recorded: String,
}

/// One stored source file, as [`ReviewAck`] lists it.
#[derive(Serialize)]
struct SourceSummary {
    name: String,
    role: &'static str,
    bytes: usize,
}

impl AppTool for Review {
    type Args = ReviewArgs;
    type Output = ReviewAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_review",
            "Record a commit on a headway card for review: its sha, title, branch, repo path and repo identity, read from git in `repo_dir`, with this host's name and the `agentium:` session that did the work. Answers `{ok, events, review, sources}`, `review` holding what was recorded.",
            vec![
                card_arg(),
                ToolArg::new(
                    "commit",
                    ToolArgType::String,
                    "The rev to record.",
                )
                .default(serde_json::json!("HEAD")),
                repo_dir_arg("The checkout to read git in."),
                ToolArg::new(
                    "explainer",
                    ToolArgType::String,
                    "URL of the explainer page for the work.",
                ),
                ToolArg::new(
                    "deploy",
                    ToolArgType::String,
                    "URL where a build of this commit is live, to open and try.",
                ),
                ToolArg::new(
                    "remote",
                    ToolArgType::String,
                    "An explicit fetch URL for the commit, overriding the one derived from the host.",
                ),
                ToolArg::new(
                    "agentium",
                    ToolArgType::String,
                    "The session that did the work, `agentium:<word-id>`; refused when it isn't one.",
                ),
                ToolArg::new(
                    "default_agentium",
                    ToolArgType::String,
                    "The session to record when `agentium` isn't given; left out when it isn't an `agentium:<word-id>` ref.",
                )
                .env(AGENTIUM_ENV),
                ToolArg::new(
                    "explainer_source",
                    ToolArgType::Paths,
                    "The explainer's Markdown source files to store on the record (absolute paths, 32 KiB of text each), named by their file names.",
                ),
                ToolArg::new(
                    "code_source",
                    ToolArgType::Paths,
                    "The code-walk files to store on the record (absolute paths, 32 KiB of text each), named by their file names.",
                ),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Record review")
        .command(&["headway", "review"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        // Read the files and git before the board, so a bad path or rev fails
        // fast, as the CLI's parse-time gathering does.
        let flags: Vec<SourceFlag> = sources(ReviewSourceRole::Explainer, args.explainer_source)
            .chain(sources(ReviewSourceRole::Code, args.code_source))
            .collect();
        let sources = review::read_sources(&flags)?;
        let fields = review::gather(ReviewFlags {
            commit: args.commit,
            explainer: args.explainer,
            deploy: args.deploy,
            agentium: args.agentium,
            default_agentium: args.default_agentium,
            remote: args.remote,
            repo_dir: Some(repo_dir(args.repo_dir)?),
            sources: flags,
        })?;
        let recorded = review::recorded_text(&fields, &sources);
        let review = fields_json(&fields);
        let summaries = sources.iter().map(SourceSummary::of).collect();
        let card = args.card;
        let ack = mutate(cx, &board, |view| {
            Ok(BoardAction::AddReview {
                card: resolve_card(view, &card)?,
                review: fields,
                sources,
            })
        })?;
        Ok(ReviewAck {
            ack,
            review,
            sources: summaries,
            recorded,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!("{}{}", out.ack.render(), out.recorded))
    }
}

/// `paths` as the [`SourceFlag`]s of `role`, in order.
fn sources(role: ReviewSourceRole, paths: Vec<String>) -> impl Iterator<Item = SourceFlag> {
    paths.into_iter().map(move |path| SourceFlag { role, path })
}

/// `fields` as a JSON object of the tags it sets.
fn fields_json(fields: &ReviewFields) -> serde_json::Map<String, serde_json::Value> {
    fields
        .tags()
        .into_iter()
        .filter_map(|tag| Some((tag.name.to_owned(), tag.value?.into())))
        .collect()
}

impl SourceSummary {
    /// `source` without its text.
    fn of(source: &ReviewSource) -> Self {
        Self {
            name: source.name.clone(),
            role: source.role.as_str(),
            bytes: source.text.len(),
        }
    }
}

/// `headway_diff`: a card's reviewed commit, found (or fetched) on this host
/// and read back `git show`-style, or one of its explain notes. Slow: a
/// fetch from the host that recorded it can take a minute, so it runs off
/// the render loop.
pub(super) struct Diff;

/// Arguments for [`Diff`].
#[derive(Deserialize)]
pub(super) struct DiffArgs {
    card: String,
    #[serde(default)]
    record: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    repo_dir: Option<String>,
    #[serde(flatten)]
    route: Route,
}

/// What [`Diff`] read back.
pub(super) struct DiffAnswer(DiffOut);

impl Serialize for DiffAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let DiffOut { resolved, body } = &self.0;
        let mut value = serde_json::json!({
            "commit": resolved.sha,
            "repo_dir": resolved.repo_dir,
            "found": resolved.source_label(),
            "resolved": resolved.to_string(),
        });
        match body {
            DiffBody::Patch(patch) => {
                value["author"] = patch.author.clone().into();
                value["date"] = patch.date.clone().into();
                value["message"] = patch.message.clone().into();
                value["patch"] = patch.patch.clone().into();
                value["truncated"] = patch.truncated.into();
            }
            DiffBody::Note { kind, text } => {
                value["note"] = kind.name().into();
                value["text"] = text.clone().into();
            }
        }
        value.serialize(serializer)
    }
}

impl SlowTool for Diff {
    type Args = DiffArgs;
    type Output = DiffAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_diff",
            "Read back the commit a headway card's review record names, `git show`-style: from a checkout on this host, else fetched from the host that recorded it into a headway-owned cache. A card with no record is searched by its `Headway:` trailer in `repo_dir`. Answers `{commit, repo_dir, found, resolved, author, date, message, patch, truncated}`, or `{…, note, text}` with `note`.",
            vec![
                card_arg(),
                ToolArg::new(
                    "record",
                    ToolArgType::String,
                    "The review record whose commit starts with this. Defaults to the newest.",
                ),
                ToolArg::new(
                    "note",
                    ToolArgType::Enum(NoteKind::ALL.iter().map(|k| k.name()).collect()),
                    "Read this explain note of the commit instead of its patch.",
                ),
                repo_dir_arg("The checkout to try first, and to search by trailer."),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Show diff")
        .read_only()
        .command(&["headway", "diff"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<DiffAnswer>, String> {
        let board = args.route.board([args.card.as_str()])?;
        let note = match args.note.as_deref() {
            Some(name) => Some(
                NoteKind::parse(name)
                    .ok_or_else(|| format!("note must be explain or code, got '{name}'"))?,
            ),
            None => None,
        };
        let cache_root = cx
            .path
            .map(crate::git_cache_root)
            .ok_or("this context has no data directory for the git cache")?;
        let view = board_view(cx, &board)?;
        let repo_dir = repo_dir(args.repo_dir)?;
        let plan = DiffPlan::new(
            &view,
            &args.card,
            args.record.as_deref(),
            note,
            Some(Path::new(&repo_dir)),
        )?;
        Ok(Box::new(move || plan.run(&cache_root).map(DiffAnswer)))
    }

    fn render(&self, out: &DiffAnswer) -> Option<String> {
        Some(out.0.text())
    }

    fn notice(&self, out: &DiffAnswer) -> Option<String> {
        Some(out.0.notice())
    }
}
