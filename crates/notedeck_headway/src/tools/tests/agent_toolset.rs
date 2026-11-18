//! Headway's tools are the native agent's Notedeck toolset as they are: an
//! agent spec naming `headway` gets the same tool definitions `nd headway`
//! runs, reads unasked and writes gated, with no wrapper per tool.

use super::*;
use agentium_core::agent_spec::{AgentSpec, Approval, Catalog};

/// A butler-shaped spec over Headway alone, its comments widened to auto.
fn butler() -> AgentSpec {
    AgentSpec::from_json(
        r#"{
          "slug": "butler",
          "model": { "provider": "anthropic", "model": "claude-sonnet-5-5" },
          "prompt": "Validate the idea.",
          "toolsets": [{ "kind": "notedeck", "apps": ["headway"] }],
          "policy": {
            "widen": [{ "tool": "headway_comment_card", "to": "auto", "why": "its findings" }]
          },
          "budget": { "max_steps": 40 }
        }"#,
    )
    .unwrap()
}

/// Every Headway tool an agent can call is offered under its own name and
/// schema, the reads run unasked, the comment the spec widened runs unasked,
/// and every other write waits for the user.
#[test]
fn headway_tools_are_the_notedeck_toolset() {
    let specs: Vec<ToolSpec> = tools().iter().map(|t| t.spec().clone()).collect();
    let catalog = Catalog::build(&butler(), &specs).unwrap();
    let approval = |name: &str| {
        catalog
            .get(name)
            .unwrap_or_else(|| panic!("{name} is offered"))
            .approval
    };

    for read in [
        "headway_show_board",
        "headway_show_card",
        "headway_grep",
        "headway_list",
        "headway_ready_items",
    ] {
        assert_eq!(approval(read), Approval::Auto, "{read}");
    }
    assert_eq!(approval("headway_comment_card"), Approval::Auto);
    for write in [
        "headway_add_card",
        "headway_move_card",
        "headway_set_description",
        "headway_block",
        "headway_delete_card",
    ] {
        assert_eq!(approval(write), Approval::Gated, "{write}");
    }

    // The model gets each tool's own definition, not a copy rewritten.
    let model_tools = catalog.model_tools();
    for spec in &specs {
        let Some(offered) = model_tools.get(spec.name()) else {
            continue;
        };
        assert_eq!(offered.spec().json_schema(), spec.json_schema());
    }
    assert_eq!(model_tools.len(), catalog.tools().len());
}
