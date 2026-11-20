//! The example specs parse, and resolve sample tools to the expected
//! approval.

use super::*;
use crate::tool::{Capability, ToolSpec};
use Approval::*;

const BUTLER: &str = include_str!("builtin/butler.json");
const RESEARCHER: &str = include_str!("builtin/researcher.json");
const REVIEWER: &str = include_str!("examples/reviewer.json");
const CODER: &str = include_str!("examples/coder.json");

fn spec(json: &str) -> AgentSpec {
    AgentSpec::from_json(json).expect("example spec parses and validates")
}

/// The toolset of `kind` in `spec`.
fn toolset(spec: &AgentSpec, kind: ToolsetKind) -> &Toolset {
    spec.toolsets
        .iter()
        .find(|t| t.kind() == kind)
        .expect("spec has the toolset")
}

/// Sample app tools, declared as headway's are: queries read-only, writes
/// left to the derived class.
fn show_card() -> ToolSpec {
    ToolSpec::new("headway_show_card", "Show a card.", vec![]).read_only()
}
fn comment_card() -> ToolSpec {
    ToolSpec::new("headway_comment_card", "Comment on a card.", vec![])
}
fn move_card() -> ToolSpec {
    ToolSpec::new("headway_move_card", "Move a card.", vec![])
}
fn open_card() -> ToolSpec {
    ToolSpec::new("headway_open_card", "Open a card.", vec![]).ui()
}

/// Sample coding tools, which declare their class.
fn read() -> ToolSpec {
    ToolSpec::new("Read", "Read a file.", vec![]).read_only()
}
fn edit() -> ToolSpec {
    ToolSpec::new("Edit", "Edit a file.", vec![]).capability(Capability::MutateWorkspace)
}
fn bash() -> ToolSpec {
    super::coding::bash()
}
fn git_commit() -> ToolSpec {
    ToolSpec::new("git_commit", "Commit.", vec![]).capability(Capability::MutateSharedRepo)
}

/// Sample web tools.
fn web_fetch() -> ToolSpec {
    ToolSpec::new("web_fetch", "Fetch a URL.", vec![]).capability(Capability::Egress)
}
fn web_search() -> ToolSpec {
    ToolSpec::new("web_search", "Search the web.", vec![])
        .read_only()
        .capability(Capability::Egress)
}

#[test]
fn butler_reads_and_comments_unasked_and_asks_for_every_other_write() {
    let butler = spec(BUTLER);
    let notedeck = toolset(&butler, ToolsetKind::Notedeck);
    assert_eq!(butler.approval(notedeck, &show_card()), Auto);
    assert_eq!(butler.approval(notedeck, &open_card()), Auto);
    assert_eq!(butler.approval(notedeck, &comment_card()), Auto);
    assert_eq!(butler.approval(notedeck, &move_card()), Gated);
    assert_eq!(butler.containment(), Containment::InProcess);
}

/// The butler's question to its researcher leaves the private context for
/// one that reaches the network, so the call is classed as egress and is
/// gated unless the spec names it.
#[test]
fn butler_asks_before_each_research_question() {
    let butler = spec(BUTLER);
    assert_eq!(
        butler.approval_for(
            super::web::ASK_RESEARCHER,
            Capability::Egress,
            Containment::InProcess
        ),
        Gated
    );
}

#[test]
fn researcher_searches_and_fetches_unasked_but_has_nothing_private() {
    let researcher = spec(RESEARCHER);
    let web = toolset(&researcher, ToolsetKind::Web);
    assert_eq!(researcher.approval(web, &web_search()), Auto);
    assert_eq!(researcher.approval(web, &web_fetch()), Auto);
    assert!(!researcher.toolsets.iter().any(Toolset::holds_private_data));
}

/// Only a spec with nothing private may be a sub-agent: the researcher
/// may, the butler and the reviewer may not, and neither may a spec that
/// calls a sub-agent of its own.
#[test]
fn only_a_spec_with_nothing_private_is_a_sub_agent() {
    spec(RESEARCHER).check_sub_agent().unwrap();
    for json in [BUTLER, REVIEWER, CODER] {
        assert!(matches!(
            spec(json).check_sub_agent(),
            Err(SpecError::SubAgentHoldsPrivateData(_))
        ));
    }
    let mut nests = spec(RESEARCHER);
    nests.toolsets.push(Toolset::SubAgent {
        spec: "other".into(),
    });
    assert!(matches!(
        nests.check_sub_agent(),
        Err(SpecError::NestedSubAgent(_))
    ));
}

#[test]
fn reviewer_observes_and_can_change_nothing() {
    let reviewer = spec(REVIEWER);
    let coding = toolset(&reviewer, ToolsetKind::Coding);
    let notedeck = toolset(&reviewer, ToolsetKind::Notedeck);
    assert_eq!(coding.containment(), Containment::Host);
    assert_eq!(reviewer.approval(coding, &read()), Auto);
    assert_eq!(reviewer.approval(coding, &edit()), Impossible);
    assert_eq!(reviewer.approval(coding, &bash()), Impossible);
    assert_eq!(reviewer.approval(coding, &git_commit()), Impossible);
    assert_eq!(reviewer.approval(notedeck, &show_card()), Auto);
    assert_eq!(reviewer.approval(notedeck, &comment_card()), Impossible);
}

/// The coder as shipped runs on the host at full power, like Claude Code:
/// the table gates its edits and commits, and the session's permission mode
/// decides what actually asks.
#[test]
fn the_coder_runs_on_the_host_and_the_mode_decides() {
    let coder = spec(CODER);
    let coding = toolset(&coder, ToolsetKind::Coding);
    assert_eq!(coding.containment(), Containment::Host);
    assert_eq!(coder.approval(coding, &read()), Auto);
    assert_eq!(coder.approval(coding, &edit()), Gated);
    assert_eq!(coder.approval(coding, &git_commit()), Gated);
    assert_eq!(
        coder.session_tags(),
        SessionSpecTags {
            spec: "coder",
            containment: Containment::Host,
        }
    );

    let edit_in = |mode| {
        coder
            .policy
            .resolve_in("Edit", Capability::MutateWorkspace, Containment::Host, mode)
    };
    assert_eq!(edit_in(SessionMode::Manual), Gated);
    assert_eq!(edit_in(SessionMode::AcceptEdits), Auto);
    assert_eq!(edit_in(SessionMode::Auto), Auto);
    assert_eq!(edit_in(SessionMode::Plan), Impossible);

    // The shell is not an edit: Accept Edits and Auto still ask before it,
    // as Claude Code's do, and Plan refuses it.
    let bash_in = |mode| {
        coder
            .policy
            .resolve_in("Bash", Capability::Shell, Containment::Host, mode)
    };
    assert_eq!(coder.approval(coding, &bash()), Gated);
    assert_eq!(bash_in(SessionMode::AcceptEdits), Gated);
    assert_eq!(bash_in(SessionMode::Auto), Gated);
    assert_eq!(bash_in(SessionMode::Plan), Impossible);
}

/// The same coder in a review guest edits and runs unasked but can't
/// commit: there, containment is what relaxed it.
#[test]
fn the_coder_in_a_guest_edits_unasked_but_cannot_commit() {
    let mut coder = spec(CODER);
    coder.toolsets = vec![Toolset::Coding {
        runner: Runner::Microsandbox {
            git: SandboxGit::ReadOnly,
        },
    }];
    let coding = toolset(&coder, ToolsetKind::Coding);
    assert_eq!(coding.containment(), Containment::SandboxedReview);
    assert_eq!(coder.approval(coding, &edit()), Auto);
    assert_eq!(coder.approval(coding, &bash()), Auto);
    assert_eq!(coder.approval(coding, &git_commit()), Impossible);
}

#[test]
fn a_spec_round_trips_through_json() {
    let butler = spec(BUTLER);
    let json = serde_json::to_string(&butler).unwrap();
    assert_eq!(AgentSpec::from_json(&json).unwrap(), butler);
}

/// The butler with web tools of its own, rather than through the
/// researcher, is the exfiltration path the sub-agent exists to close.
#[test]
fn private_data_beside_egress_is_refused() {
    let mut butler = spec(BUTLER);
    butler.toolsets.push(Toolset::Web);
    assert!(matches!(
        butler.validate(),
        Err(SpecError::PrivateDataWithEgress(_))
    ));

    // A coding toolset holds a private repository just the same.
    let mut coder = spec(CODER);
    coder.toolsets.push(Toolset::Web);
    assert!(matches!(
        coder.validate(),
        Err(SpecError::PrivateDataWithEgress(_))
    ));
}

/// The reviewer as shipped: its class tightens make Bash impossible, so its
/// host shell reaches nothing and may sit beside private boards.
#[test]
fn the_reviewers_closed_shell_may_sit_beside_private_boards() {
    let reviewer = spec(REVIEWER);
    let coding = toolset(&reviewer, ToolsetKind::Coding);
    assert!(coding.has_egress());
    assert!(!reviewer.reaches_network(coding));
}

/// The reviewer without its tightens runs a host shell that can `curl`
/// beside the boards it reads: the exfiltration path validate exists to
/// close, whether Bash is gated or widened to run unasked.
#[test]
fn a_host_shell_beside_private_boards_is_refused() {
    let mut open = spec(REVIEWER);
    open.policy.tighten.clear();
    assert!(matches!(
        open.validate(),
        Err(SpecError::PrivateDataWithShell(_))
    ));
    open.policy.widen.push(Widen {
        tool: "Bash".into(),
        to: Auto,
        why: "review runs the tests".into(),
    });
    assert!(matches!(
        open.validate(),
        Err(SpecError::PrivateDataWithShell(_))
    ));

    // Gating Bash still leaves the network one click away.
    let mut gated = open.clone();
    gated.policy.tighten.push(Tighten {
        tool: Some("Bash".into()),
        class: None,
        to: Gated,
    });
    assert!(matches!(
        gated.validate(),
        Err(SpecError::PrivateDataWithShell(_))
    ));

    // Naming Bash impossible closes it, and beats the widen.
    let mut closed = open;
    closed.policy.tighten.push(Tighten {
        tool: Some("Bash".into()),
        class: None,
        to: Impossible,
    });
    closed.validate().unwrap();
}

/// A guest shell reaches the network too: no runner gives a guest none yet.
#[test]
fn a_guest_shell_beside_private_boards_is_refused() {
    let mut coder = spec(CODER);
    coder.toolsets = vec![Toolset::Coding {
        runner: Runner::Microsandbox {
            git: SandboxGit::ReadOnly,
        },
    }];
    coder.toolsets.push(Toolset::Notedeck {
        apps: vec!["headway".into()],
    });
    assert!(matches!(
        coder.validate(),
        Err(SpecError::PrivateDataWithShell(_))
    ));
}

/// A coder's own repository beside its own shell is the full-power host
/// coder jb55 chose: accepted, on either runner.
#[test]
fn a_coder_alone_keeps_its_shell() {
    let mut coder = spec(CODER);
    coder.validate().unwrap();
    assert!(coder.reaches_network(&coder.toolsets[0]));
    coder.toolsets = vec![Toolset::Coding {
        runner: Runner::Microsandbox {
            git: SandboxGit::ReadOnly,
        },
    }];
    coder.validate().unwrap();
}

#[test]
fn a_misspelled_field_is_an_error_not_a_dropped_rule() {
    let typo = BUTLER.replace("\"widen\"", "\"widden\"");
    assert!(matches!(
        AgentSpec::from_json(&typo),
        Err(SpecError::Parse(_))
    ));
}

#[test]
fn validate_rejects_malformed_specs() {
    let base = spec(BUTLER);
    let check = |edit: fn(&mut AgentSpec)| {
        let mut s = base.clone();
        edit(&mut s);
        s.validate().unwrap_err()
    };
    assert!(matches!(
        check(|s| s.slug = "Butler".into()),
        SpecError::Slug(_)
    ));
    assert!(matches!(
        check(|s| s.model.provider.clear()),
        SpecError::Model(_)
    ));
    assert!(matches!(
        check(|s| s.toolsets.clear()),
        SpecError::NoToolsets(_)
    ));
    assert!(matches!(
        check(|s| s.toolsets.push(Toolset::Notedeck {
            apps: vec!["headway".into()]
        })),
        SpecError::DuplicateToolset {
            kind: ToolsetKind::Notedeck,
            ..
        }
    ));
    assert!(matches!(
        check(|s| s.toolsets[0] = Toolset::Notedeck { apps: vec![] }),
        SpecError::NoApps(_)
    ));
    assert!(matches!(
        check(|s| s.toolsets[0] = Toolset::Notedeck {
            apps: vec![ALL_APPS.into(), "headway".into()]
        }),
        SpecError::AllAppsNotAlone(_)
    ));
    assert!(matches!(
        check(|s| s.trust = Some(Trust::Host)),
        SpecError::TrustOffHost {
            containment: Containment::InProcess,
            ..
        }
    ));
    assert!(matches!(
        check(|s| s.toolsets[1] = Toolset::SubAgent {
            spec: "butler".into()
        }),
        SpecError::SelfSubAgent(_)
    ));
    assert!(matches!(
        check(|s| s.budget.max_steps = 0),
        SpecError::ZeroSteps(_)
    ));
    assert!(matches!(
        check(|s| s.policy.widen[0].why.clear()),
        SpecError::Policy { .. }
    ));
    assert!(matches!(
        check(|s| s.triggers = vec![Trigger::CardDue {
            label: "idea".into(),
            prompt: " ".into(),
        }]),
        SpecError::IncompleteTrigger(_)
    ));
    assert!(matches!(
        check(|s| {
            s.toolsets[0] = Toolset::Notedeck {
                apps: vec!["notebook".into()],
            };
            s.triggers = vec![Trigger::CardDue {
                label: "idea".into(),
                prompt: "Review {card}.".into(),
            }];
        }),
        SpecError::TriggerWithoutHeadway(_)
    ));
}

/// Every example spec's model resolves to the built-in Anthropic endpoint
/// with no profile on the account, given the key in the env.
#[test]
fn example_models_resolve_to_a_builtin_provider() {
    let env = |var: &str| (var == "ANTHROPIC_API_KEY").then(|| "sk-ant".to_string());
    for json in [BUTLER, RESEARCHER, REVIEWER, CODER] {
        let spec = spec(json);
        let endpoint = provider::resolve(&spec.model, &Default::default(), env)
            .unwrap_or_else(|e| panic!("{}: {e}", spec.slug));
        assert_eq!(endpoint.base_url, "https://api.anthropic.com/v1");
        assert_eq!(endpoint.model, spec.model.model);
    }
}
