//! The specs notedeck ships: the ones a session can be spawned under by
//! slug (`nd agentium spawn --spec butler`), without the user writing one.
//!
//! | slug | does | toolsets |
//! |---|---|---|
//! | `butler` | validates an idea from a brainstorm transcript onto its headway card | Notedeck (headway, notebook) + the researcher |
//! | `researcher` | answers one web question for another agent | Web |
//!
//! Each lives as JSON beside this file, the same shape a user's own spec
//! takes ([`AgentSpec::from_json`]), so reading one is reading config, not
//! code.

use super::AgentSpec;

/// Every built-in spec's slug, in the order `find` knows them.
pub const SLUGS: [&str; 2] = ["butler", "researcher"];

/// The built-in spec named `slug`, parsed and validated; `None` when no
/// built-in has that slug.
pub fn find(slug: &str) -> Option<AgentSpec> {
    let json = match slug {
        "butler" => include_str!("builtin/butler.json"),
        "researcher" => include_str!("builtin/researcher.json"),
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
    use crate::agent_spec::{Toolset, ToolsetKind};

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
}
