//! The tools a spec's agent is offered, and what each call needs.
//!
//! A spec names toolsets, not tools. [`Catalog::build`] turns its toolsets
//! into the concrete tools the model sees, each with the approval its call
//! gets, from the tools the host has. Building it is also where the spec is
//! checked against what exists: an app with no tools and a policy rule
//! naming a tool nobody offers are errors, since a misspelled tighten would
//! otherwise restrict nothing and say nothing.
//!
//! ## The Notedeck toolset is the `nd` commands
//!
//! An app's tools are the ones whose command line starts with the app's
//! name (`["headway", "show"]` is `nd headway show`), so a
//! `Notedeck { apps: ["headway"] }` toolset offers exactly what `nd headway`
//! runs. No wrapper per tool: the model gets each tool's own
//! [`ToolSpec`], and the host's tool registry runs the call. A streaming
//! tool is left out, since nothing waits on a stream. An argument the
//! command bar fills from the current view
//! ([`ToolArg::from_current`](crate::tool::ToolArg::from_current)) is one
//! the model passes itself.
//!
//! ## A UI tool is offered when it has a command line
//!
//! A [UI tool](crate::tool::ToolKind::Ui) acts on the window: it opens a pane, moves a
//! cursor or routes a column. Most are a key binding's action and have no
//! command line, so no toolset reaches them. One that an app puts on the
//! `nd` surface (`nd nostr open-thread`) has been made for a caller outside the
//! window, and its toolset offers it like any other `nd <app>` command. It
//! is [`Navigate`](Capability::Navigate) unless its spec declares a class,
//! so it runs unasked. A UI tool that changes data has to declare that
//! class ([`ToolSpec::capability`]), or it runs unasked too. The registry
//! answers its call with an intent, which the host routes to the tool's
//! app.
//!
//! ## Web, sub-agent and coding tools are declared here
//!
//! The `Web`, `SubAgent` and `Coding` toolsets have no app behind them, so
//! their tools come from [`web`](super::web) and [`coding`](super::coding),
//! not the registry. Each [`CatalogTool`] says where its call goes
//! ([`ToolSource`]), so the host runs an app tool through its registry, a
//! coding tool through the toolset's runner, and the rest itself.

use std::collections::HashMap;

use super::{coding, web};
use super::{AgentSpec, Approval, Containment, Policy, SessionMode, Toolset};
use crate::tool::{Capability, ModelReach, ToolSpec};
use crate::tools::Tool;

/// One tool the agent may call.
#[derive(Debug, Clone)]
pub struct CatalogTool {
    /// What the model is shown.
    pub spec: ToolSpec,
    /// What a call needs in a [`Manual`](SessionMode::Manual) session:
    /// [`Approval::Auto`] or [`Approval::Gated`]. An impossible tool is
    /// never offered. A call in a session reads
    /// [`Catalog::approval_in`] instead, which follows the session's mode.
    pub approval: Approval,
    /// What a call of it may do: the table's row.
    pub class: Capability,
    /// Where it runs: the table's column.
    pub containment: Containment,
    /// Who runs a call of it.
    pub source: ToolSource,
}

/// Who runs a [`CatalogTool`]'s calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    /// The host's tool registry: an app's tool, run as `nd <app> …` runs it.
    App,
    /// The host's web client: [`web::web_search`] or [`web::web_fetch`].
    Web,
    /// A child loop under the spec named `spec`, asked through
    /// [`web::ask_researcher`].
    SubAgent {
        /// The child spec's slug.
        spec: String,
    },
    /// The Coding toolset's runner: a [`coding`] tool, its call parsed as a
    /// [`CodingCall`](coding::CodingCall).
    Coding,
}

/// Every tool a spec's agent is offered (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    tools: Vec<CatalogTool>,
    /// The spec's policy, for resolving a call under the session's mode.
    policy: Policy,
}

/// Why a spec's [`Catalog`] can't be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// A Notedeck toolset names an app that offers no tools.
    #[error("agent spec {spec}: no app named {app:?} offers tools")]
    UnknownApp { spec: String, app: String },
    /// A policy rule names a tool none of the spec's toolsets offers.
    #[error("agent spec {spec}: its policy names {tool:?}, which none of its toolsets offers")]
    UnknownPolicyTool { spec: String, tool: String },
}

impl Catalog {
    /// The tools `spec`'s agent is offered, from `available`: every tool
    /// the host has (the registry's specs), for its Notedeck toolset, the
    /// [`web`] tools for its Web and SubAgent toolsets, and the [`coding`]
    /// tools for its Coding toolset. Every tool gets its
    /// [`approval`](AgentSpec::approval), and the impossible ones are
    /// dropped. Fails on an app that offers nothing and a policy rule naming
    /// a tool that isn't offered.
    pub fn build<'a>(
        spec: &AgentSpec,
        available: impl IntoIterator<Item = &'a ToolSpec> + Clone,
    ) -> Result<Self, CatalogError> {
        let mut catalog = Offering {
            spec,
            tools: Vec::new(),
            offered: Vec::new(),
        };
        for toolset in &spec.toolsets {
            match toolset {
                Toolset::Notedeck { apps } => {
                    for app in apps {
                        let before = catalog.offered.len();
                        for tool in available.clone().into_iter().filter(|t| offers(app, t)) {
                            catalog.offer(toolset, tool.clone(), ToolSource::App);
                        }
                        if catalog.offered.len() == before {
                            return Err(CatalogError::UnknownApp {
                                spec: spec.slug.clone(),
                                app: app.clone(),
                            });
                        }
                    }
                }
                Toolset::Web => {
                    for tool in web::web_tools() {
                        catalog.offer(toolset, tool, ToolSource::Web);
                    }
                }
                Toolset::SubAgent { spec: child } => {
                    let source = ToolSource::SubAgent {
                        spec: child.clone(),
                    };
                    catalog.offer(toolset, web::ask_researcher(), source);
                }
                Toolset::Coding { .. } => {
                    for tool in coding::coding_tools() {
                        catalog.offer(toolset, tool, ToolSource::Coding);
                    }
                }
            }
        }
        let Offering { tools, offered, .. } = catalog;
        if let Some(tool) = policy_tools(spec).find(|name| !offered.contains(name)) {
            return Err(CatalogError::UnknownPolicyTool {
                spec: spec.slug.clone(),
                tool: tool.to_owned(),
            });
        }
        Ok(Self {
            tools,
            policy: spec.policy.clone(),
        })
    }

    /// The tool offered as `name`.
    pub fn get(&self, name: &str) -> Option<&CatalogTool> {
        self.tools.iter().find(|t| t.spec.name() == name)
    }

    /// What a call of the tool offered as `name` needs in a session running
    /// in `mode` (see [`Policy::resolve_in`]): Plan can make it
    /// [`Approval::Impossible`], which the caller tells the model rather
    /// than calling it not offered. `None` for a tool not offered.
    pub fn approval_in(&self, name: &str, mode: SessionMode) -> Option<Approval> {
        let tool = self.get(name)?;
        Some(
            self.policy
                .resolve_in(name, tool.class, tool.containment, mode),
        )
    }

    /// Every offered tool.
    pub fn tools(&self) -> &[CatalogTool] {
        &self.tools
    }

    /// The tools as a backend advertises them, keyed by name: each an
    /// [app tool](Tool::app), so its calls come back for the host's
    /// registry to run.
    pub fn model_tools(&self) -> HashMap<String, Tool> {
        self.tools
            .iter()
            .map(|t| (t.spec.name().to_owned(), Tool::app(t.spec.clone())))
            .collect()
    }
}

/// A catalog being built: what it offers so far.
struct Offering<'a> {
    spec: &'a AgentSpec,
    /// The tools the model gets.
    tools: Vec<CatalogTool>,
    /// Every tool a toolset offers, impossible ones too: a policy naming
    /// one of those is a real rule, not a typo.
    offered: Vec<&'static str>,
}

impl Offering<'_> {
    /// Offer `tool` from `toolset`, run by `source`, unless the spec makes
    /// it impossible.
    fn offer(&mut self, toolset: &Toolset, tool: ToolSpec, source: ToolSource) {
        self.offered.push(tool.name());
        let approval = self.spec.approval(toolset, &tool);
        if approval != Approval::Impossible {
            self.tools.push(CatalogTool {
                class: tool.capability_class(),
                containment: toolset.containment(),
                spec: tool,
                approval,
                source,
            });
        }
    }
}

/// Whether `app`'s Notedeck toolset offers `tool`: it is `nd <app> …`, and a
/// model can call it ([`ToolSpec::model_reach`], the rule `nd mcp` reads
/// too). A UI tool ([`ModelReach::Window`]) qualifies through its command
/// line alone (see the module docs), where `nd mcp` also wants `--ui`.
fn offers(app: &str, tool: &ToolSpec) -> bool {
    tool.command_path().first() == Some(&app) && tool.model_reach() != ModelReach::Never
}

/// Every tool `spec`'s policy names, widened or tightened.
fn policy_tools(spec: &AgentSpec) -> impl Iterator<Item = &str> {
    let widened = spec.policy.widen.iter().map(|w| w.tool.as_str());
    let tightened = spec.policy.tighten.iter().filter_map(|t| t.tool.as_deref());
    widened.chain(tightened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::{Budget, ModelRef, Policy, Tighten, Widen};
    use crate::tool::{StreamMode, ToolArg, ToolArgType};

    /// What a host with headway and notebook offers, a UI tool on the
    /// command line among them, plus tools no agent gets: a UI tool that is
    /// only a key binding's, a streaming one, and one with no command line.
    fn host_tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("headway_show_board", "Show a board.", vec![])
                .read_only()
                .command(&["headway", "show"]),
            ToolSpec::new("headway_comment_card", "Comment on a card.", vec![])
                .command(&["headway", "comment"]),
            ToolSpec::new("headway_move_card", "Move a card.", vec![])
                .command(&["headway", "move"]),
            ToolSpec::new("notebook_show", "Show the canvas.", vec![])
                .read_only()
                .command(&["notebook", "show"]),
            ToolSpec::new("headway_open", "Open a card in the window.", vec![])
                .ui()
                .command(&["headway", "open"]),
            ToolSpec::new("headway_cursor_down", "Move the cursor down.", vec![]).ui(),
            ToolSpec::new("headway_watch", "Follow a board.", vec![])
                .read_only()
                .streams(StreamMode::Append)
                .command(&["headway", "watch"]),
            ToolSpec::new("query", "Search notes.", vec![]).read_only(),
        ]
    }

    fn spec(apps: &[&str], policy: Policy) -> AgentSpec {
        AgentSpec {
            slug: "butler".into(),
            model: ModelRef {
                provider: "anthropic".into(),
                model: "claude-sonnet-5-5".into(),
            },
            prompt: "Validate ideas.".into(),
            toolsets: vec![Toolset::Notedeck {
                apps: apps.iter().map(|a| a.to_string()).collect(),
            }],
            policy,
            budget: Budget {
                max_steps: 10,
                max_cost_cents: None,
                timeout_secs: None,
            },
        }
    }

    fn names(catalog: &Catalog) -> Vec<&'static str> {
        catalog.tools().iter().map(|t| t.spec.name()).collect()
    }

    /// An app's toolset is its `nd <app>` commands, minus what a model
    /// can't call; reads and window moves run unasked, writes are gated.
    #[test]
    fn an_apps_toolset_is_its_nd_commands_reads_auto_writes_gated() {
        let host = host_tools();
        let catalog = Catalog::build(&spec(&["headway"], Policy::default()), &host).unwrap();
        assert_eq!(
            names(&catalog),
            [
                "headway_show_board",
                "headway_comment_card",
                "headway_move_card",
                "headway_open",
            ]
        );
        let approval = |name| catalog.get(name).unwrap().approval;
        assert_eq!(approval("headway_show_board"), Approval::Auto);
        assert_eq!(approval("headway_comment_card"), Approval::Gated);
        assert_eq!(approval("headway_move_card"), Approval::Gated);
        assert_eq!(approval("headway_open"), Approval::Auto);
        assert_eq!(
            catalog.get("headway_open").unwrap().class,
            Capability::Navigate
        );
        assert!(
            catalog.get("headway_cursor_down").is_none(),
            "a UI tool with no command line is a key binding's"
        );
        assert!(
            catalog.get("notebook_show").is_none(),
            "notebook isn't named"
        );
    }

    /// Two apps offer both apps' tools, and the model gets each as an app
    /// tool under its own name.
    #[test]
    fn two_apps_offer_both_and_advertise_as_app_tools() {
        let host = host_tools();
        let catalog =
            Catalog::build(&spec(&["headway", "notebook"], Policy::default()), &host).unwrap();
        let tools = catalog.model_tools();
        assert_eq!(tools.len(), 5);
        let mut call = crate::tools::PartialToolCall::default();
        *call.id_mut() = Some("1".into());
        *call.name_mut() = Some("notebook_show".into());
        *call.arguments_mut() = Some("{}".into());
        let parsed = call.complete().unwrap().parse(&tools).unwrap();
        let crate::tools::ToolCalls::App(app) = parsed.calls() else {
            panic!("an app tool's call parses as an app call");
        };
        assert_eq!(app.name, "notebook_show");
    }

    /// The butler's policy widens comments alone to auto.
    #[test]
    fn a_widen_runs_comments_unasked_and_nothing_else() {
        let host = host_tools();
        let policy = Policy {
            widen: vec![Widen {
                tool: "headway_comment_card".into(),
                to: Approval::Auto,
                why: "its findings are comments".into(),
            }],
            tighten: vec![],
        };
        let catalog = Catalog::build(&spec(&["headway"], policy), &host).unwrap();
        assert_eq!(
            catalog.get("headway_comment_card").unwrap().approval,
            Approval::Auto
        );
        assert_eq!(
            catalog.get("headway_move_card").unwrap().approval,
            Approval::Gated
        );
    }

    /// A tool tightened to impossible isn't offered at all.
    #[test]
    fn an_impossible_tool_is_not_offered() {
        let host = host_tools();
        let policy = Policy {
            widen: vec![],
            tighten: vec![Tighten {
                tool: Some("headway_move_card".into()),
                class: None,
                to: Approval::Impossible,
            }],
        };
        let catalog = Catalog::build(&spec(&["headway"], policy), &host).unwrap();
        assert!(catalog.get("headway_move_card").is_none());
        assert!(!catalog.model_tools().contains_key("headway_move_card"));
    }

    /// A misspelled tool in a tighten is an error, not a rule that
    /// restricts nothing.
    #[test]
    fn a_policy_naming_an_unoffered_tool_is_an_error() {
        let host = host_tools();
        let policy = Policy {
            widen: vec![],
            tighten: vec![Tighten {
                tool: Some("headway_move_cards".into()),
                class: None,
                to: Approval::Impossible,
            }],
        };
        let err = Catalog::build(&spec(&["headway"], policy), &host).unwrap_err();
        assert_eq!(
            err,
            CatalogError::UnknownPolicyTool {
                spec: "butler".into(),
                tool: "headway_move_cards".into()
            }
        );

        // A tool of an app the spec doesn't name is just as unoffered.
        let policy = Policy {
            widen: vec![Widen {
                tool: "notebook_show".into(),
                to: Approval::Auto,
                why: "reads".into(),
            }],
            tighten: vec![],
        };
        assert!(matches!(
            Catalog::build(&spec(&["headway"], policy), &host),
            Err(CatalogError::UnknownPolicyTool { .. })
        ));
    }

    /// An app that offers no tools is a typo in the spec.
    #[test]
    fn an_app_with_no_tools_is_an_error() {
        let host = host_tools();
        let err = Catalog::build(&spec(&["headwya"], Policy::default()), &host).unwrap_err();
        assert_eq!(
            err,
            CatalogError::UnknownApp {
                spec: "butler".into(),
                app: "headwya".into()
            }
        );
    }

    /// The coder is offered the six coding tools, run by its runner. On
    /// the host, reads run unasked and edits and the shell ask; under
    /// Accept Edits the edits stop asking but the shell doesn't, and Plan
    /// refuses both.
    #[test]
    fn the_coder_is_offered_the_coding_tools() {
        let coder = AgentSpec::from_json(include_str!("examples/coder.json")).unwrap();
        let catalog = Catalog::build(&coder, &host_tools()).unwrap();
        assert_eq!(
            names(&catalog),
            [
                coding::READ,
                coding::WRITE,
                coding::EDIT,
                coding::GREP,
                coding::GLOB,
                coding::BASH
            ]
        );
        assert!(catalog
            .tools()
            .iter()
            .all(|t| t.source == ToolSource::Coding && t.containment == Containment::Host));
        let approval = |name, mode| catalog.approval_in(name, mode).unwrap();
        use SessionMode::*;
        assert_eq!(approval(coding::GREP, Manual), Approval::Auto);
        assert_eq!(approval(coding::EDIT, Manual), Approval::Gated);
        assert_eq!(approval(coding::BASH, Manual), Approval::Gated);
        assert_eq!(approval(coding::EDIT, AcceptEdits), Approval::Auto);
        assert_eq!(approval(coding::BASH, AcceptEdits), Approval::Gated);
        assert_eq!(approval(coding::BASH, Auto), Approval::Gated);
        assert_eq!(approval(coding::READ, Plan), Approval::Auto);
        assert_eq!(approval(coding::EDIT, Plan), Approval::Impossible);
        assert_eq!(approval(coding::BASH, Plan), Approval::Impossible);
        assert_eq!(approval(coding::BASH, Bypass), Approval::Auto);
    }

    /// The reviewer's class tightens drop every coding tool that changes
    /// anything, the shell included: it is offered the three reads.
    #[test]
    fn the_reviewer_is_offered_only_the_coding_reads() {
        let reviewer = AgentSpec::from_json(include_str!("examples/reviewer.json")).unwrap();
        let catalog = Catalog::build(&reviewer, &host_tools()).unwrap();
        let coding: Vec<_> = catalog
            .tools()
            .iter()
            .filter(|t| t.source == ToolSource::Coding)
            .map(|t| t.spec.name())
            .collect();
        assert_eq!(coding, [coding::READ, coding::GREP, coding::GLOB]);
    }

    /// The butler's sub-agent toolset offers one tool, gated: the question
    /// leaves the private context. It is run by a child loop, not the
    /// registry.
    #[test]
    fn a_sub_agent_toolset_offers_one_gated_question() {
        let host = host_tools();
        let mut butler = spec(&["headway"], Policy::default());
        butler.toolsets.push(Toolset::SubAgent {
            spec: "researcher".into(),
        });
        let catalog = Catalog::build(&butler, &host).unwrap();
        let ask = catalog.get(web::ASK_RESEARCHER).unwrap();
        assert_eq!(ask.approval, Approval::Gated);
        assert_eq!(
            ask.source,
            ToolSource::SubAgent {
                spec: "researcher".into()
            }
        );
        assert_eq!(
            catalog.get("headway_show_board").unwrap().source,
            ToolSource::App
        );
        assert!(catalog.get(web::WEB_FETCH).is_none(), "no web of its own");
    }

    /// The researcher's web tools are gated by the table and run unasked
    /// only because its spec widens them by name, which the catalog accepts
    /// since it offers them.
    #[test]
    fn the_researcher_is_offered_web_tools_it_widened() {
        let researcher = AgentSpec::from_json(include_str!("builtin/researcher.json")).unwrap();
        let catalog = Catalog::build(&researcher, &host_tools()).unwrap();
        assert_eq!(names(&catalog), [web::WEB_SEARCH, web::WEB_FETCH]);
        for tool in catalog.tools() {
            assert_eq!(tool.approval, Approval::Auto, "{}", tool.spec.name());
            assert_eq!(tool.source, ToolSource::Web);
        }

        let mut unwidened = researcher.clone();
        unwidened.policy = Policy::default();
        let catalog = Catalog::build(&unwidened, &host_tools()).unwrap();
        assert!(catalog
            .tools()
            .iter()
            .all(|t| t.approval == Approval::Gated));
    }

    /// Positional and flag metadata reaches the model's schema unchanged:
    /// the catalog clones the tool's own spec, it doesn't rewrite it.
    #[test]
    fn the_model_gets_the_tools_own_schema() {
        let arg = ToolArg::new("board", ToolArgType::String, "The board.")
            .required(true)
            .positional();
        let tool = ToolSpec::new("headway_grep", "Search cards.", vec![arg])
            .read_only()
            .command(&["headway", "grep"]);
        let host = [tool.clone()];
        let catalog = Catalog::build(&spec(&["headway"], Policy::default()), &host).unwrap();
        let offered = &catalog.model_tools()["headway_grep"];
        assert_eq!(offered.spec().json_schema(), tool.json_schema());
    }
}
