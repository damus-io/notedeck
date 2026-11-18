//! The example specs parse, and resolve sample tools to the expected
//! approval.

use super::*;
use crate::tool::{Capability, ToolSpec};
use Approval::*;

const BUTLER: &str = include_str!("examples/butler.json");
const RESEARCHER: &str = include_str!("examples/researcher.json");
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
    ToolSpec::new("Bash", "Run a command.", vec![]).capability(Capability::MutateWorkspace)
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
        butler.approval_for("ask_researcher", Capability::Egress, Containment::InProcess),
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

#[test]
fn coder_edits_and_runs_unasked_in_the_guest_but_cannot_commit() {
    let coder = spec(CODER);
    let coding = toolset(&coder, ToolsetKind::Coding);
    assert_eq!(coding.containment(), Containment::SandboxedReview);
    assert_eq!(coder.approval(coding, &read()), Auto);
    assert_eq!(coder.approval(coding, &edit()), Auto);
    assert_eq!(coder.approval(coding, &bash()), Auto);
    assert_eq!(coder.approval(coding, &git_commit()), Impossible);
    assert_eq!(
        coder.session_tags(),
        SessionSpecTags {
            spec: "coder",
            containment: Containment::SandboxedReview,
        }
    );
}

/// The same coder run on the host has to ask for every edit: containment,
/// not the spec, is what relaxed it.
#[test]
fn the_coder_on_the_host_asks_for_every_edit() {
    let mut coder = spec(CODER);
    coder.toolsets = vec![Toolset::Coding {
        runner: Runner::LocalExecutor,
    }];
    let coding = toolset(&coder, ToolsetKind::Coding);
    assert_eq!(coder.approval(coding, &edit()), Gated);
    assert_eq!(coder.approval(coding, &git_commit()), Gated);
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
}
