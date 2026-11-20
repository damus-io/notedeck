//! The specs notedeck ships: the ones a session can be spawned under by
//! slug (`nd agentium spawn --spec butler`), without the user writing one.
//!
//! | slug | does | toolsets |
//! |---|---|---|
//! | `butler` | validates an idea from a brainstorm transcript onto its headway card | Notedeck (headway, notebook) + the researcher |
//! | `researcher` | answers one web question for another agent | Web |
//! | `dave-code` | "Dave Code": codes in the session's checkout, on the host, like Claude Code | Coding on the host + Notedeck (every app), `trust: host` |
//!
//! Each lives as JSON beside this file, the same shape a user's own spec
//! takes ([`AgentSpec::from_json`]), so reading one is reading config, not
//! code.

use super::AgentSpec;

/// The label the butler gives an idea card, and its due date is the card's next
/// review: a Dave host re-runs the butler on every card carrying it when that
/// date arrives (`notedeck_dave`'s `review_schedule`). The butler's prompt
/// names it, so the two cannot drift apart unnoticed (a test checks).
pub const REVIEW_LABEL: &str = "butler";

/// Every built-in spec's slug, in the order `find` knows them.
pub const SLUGS: [&str; 3] = ["butler", "researcher", "dave-code"];

/// The built-in spec named `slug`, parsed and validated; `None` when no
/// built-in has that slug.
pub fn find(slug: &str) -> Option<AgentSpec> {
    let json = match slug {
        "butler" => include_str!("builtin/butler.json"),
        "researcher" => include_str!("builtin/researcher.json"),
        "dave-code" => include_str!("builtin/dave-code.json"),
        _ => return None,
    };
    // The tests below parse every built-in, so a broken one fails CI rather
    // than a user's spawn.
    match AgentSpec::from_json(json) {
        Ok(spec) => Some(spec),
        Err(err) => {
            tracing::error!("built-in agent spec {slug:?} is invalid: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::{
        Containment, Runner, SandboxGit, SpecError, Toolset, ToolsetKind, Trust, ALL_APPS,
    };

    /// Every built-in parses, validates, and is found under its own slug.
    #[test]
    fn every_builtin_is_found_under_its_slug() {
        for slug in SLUGS {
            let spec = find(slug).unwrap_or_else(|| panic!("built-in {slug} parses"));
            assert_eq!(spec.slug, slug);
        }
        assert!(find("scout").is_none());
    }

    /// The butler reads private boards, so its research goes through a
    /// built-in sub-agent fit to be one: no private data, no web of its own.
    #[test]
    fn the_butlers_researcher_is_a_builtin_fit_to_be_a_sub_agent() {
        let butler = find("butler").unwrap();
        assert!(
            butler.toolsets.iter().all(|t| t.kind() != ToolsetKind::Web),
            "the butler never fetches itself"
        );
        let child = butler
            .toolsets
            .iter()
            .find_map(|t| match t {
                Toolset::SubAgent { spec } => Some(spec.as_str()),
                _ => None,
            })
            .expect("the butler has a sub-agent");
        let researcher = find(child).expect("its sub-agent is a built-in");
        researcher.check_sub_agent().expect("fit to be a sub-agent");
    }

    /// The butler's prompt carries every section of the idea template the
    /// card asks for, in order.
    #[test]
    fn the_butlers_prompt_carries_the_idea_template() {
        let prompt = find("butler").unwrap().prompt;
        let sections = [
            "### 1. Legal and jurisdiction",
            "### 2. Competitors",
            "### 3. Effort, cost, return",
            "### 4. Kill criteria and time-box",
            "### 5. Review",
        ];
        let mut from = 0;
        for section in sections {
            let at = prompt[from..]
                .find(section)
                .unwrap_or_else(|| panic!("{section} missing or out of order"));
            from += at + section.len();
        }
        for grade in ["evidence moved", "no change", "killed"] {
            assert!(prompt.contains(grade), "review grade {grade}");
        }
    }

    /// The butler labels its idea cards with the label the review schedule
    /// re-runs it on, and sets the due date that schedule reads.
    #[test]
    fn the_butler_labels_its_cards_for_scheduled_reviews() {
        let prompt = find("butler").unwrap().prompt;
        assert!(prompt.contains(&format!("label `{REVIEW_LABEL}`")));
        assert!(prompt.contains("headway_set_due"));
    }

    /// Dave Code is the user's own coder: the Coding toolset on the host
    /// and every app's notedeck tools, named for the picker, its tools
    /// running on this machine. It validates only through its explicit
    /// `trust: host`.
    #[test]
    fn dave_code_codes_on_the_host_with_every_notedeck_tool() {
        let spec = find("dave-code").expect("dave-code parses");
        assert_eq!(spec.display_name(), "Dave Code");
        assert_eq!(spec.containment(), Containment::Host);
        assert_eq!(spec.trust, Some(Trust::Host));
        assert!(spec.toolsets.contains(&Toolset::Coding {
            runner: Runner::LocalExecutor
        }));
        assert!(spec.toolsets.contains(&Toolset::Notedeck {
            apps: vec![ALL_APPS.into()]
        }));
    }

    /// The same toolsets without the opt-in are refused as private data
    /// beside a shell, and the opt-in on a sandboxed runner is refused
    /// outright: it is for the host only.
    #[test]
    fn dave_codes_pairing_needs_trust_and_the_host() {
        let mut untrusted = find("dave-code").unwrap();
        untrusted.trust = None;
        assert!(matches!(
            untrusted.validate(),
            Err(SpecError::PrivateDataWithShell(_))
        ));

        let mut sandboxed = find("dave-code").unwrap();
        sandboxed.toolsets[0] = Toolset::Coding {
            runner: Runner::Microsandbox {
                git: SandboxGit::ReadWrite,
            },
        };
        assert!(matches!(
            sandboxed.validate(),
            Err(SpecError::TrustOffHost {
                containment: Containment::SandboxedCommit,
                ..
            })
        ));
    }

    /// Trust on the host never lets a Web toolset in beside private data:
    /// it widens the host shell's rule only.
    #[test]
    fn host_trust_never_admits_web_beside_private_data() {
        let mut spec = find("dave-code").unwrap();
        spec.toolsets.push(Toolset::Web);
        assert!(matches!(
            spec.validate(),
            Err(SpecError::PrivateDataWithEgress(_))
        ));
    }
}
