//! Integration test for tool result metadata display
//!
//! Tests that tool results are captured from the message stream
//! by correlating ToolUse and ToolResult content blocks.

use claude_agent_sdk_rs::{ContentBlock, ToolResultBlock, ToolResultContent, ToolUseBlock};
use std::collections::HashMap;

/// Unit test that verifies ToolUse and ToolResult correlation logic
#[test]
fn test_tool_use_result_correlation() {
    // Simulate the pending_tools tracking
    let mut pending_tools: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut tool_results: Vec<(String, String, serde_json::Value)> = Vec::new();

    // Simulate receiving a ToolUse block in an Assistant message
    let tool_use = ToolUseBlock {
        id: "toolu_123".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/etc/hostname"}),
    };

    // Store the tool use (as the main code does)
    pending_tools.insert(
        tool_use.id.clone(),
        (tool_use.name.clone(), tool_use.input.clone()),
    );

    assert_eq!(pending_tools.len(), 1);
    assert!(pending_tools.contains_key("toolu_123"));

    // Simulate receiving a ToolResult block in a User message
    let tool_result = ToolResultBlock {
        tool_use_id: "toolu_123".to_string(),
        content: Some(ToolResultContent::Text("hostname content".to_string())),
        is_error: Some(false),
    };

    // Correlate the result (as the main code does)
    if let Some((tool_name, _tool_input)) = pending_tools.remove(&tool_result.tool_use_id) {
        let response = match &tool_result.content {
            Some(ToolResultContent::Text(s)) => serde_json::Value::String(s.clone()),
            Some(ToolResultContent::Blocks(blocks)) => {
                serde_json::Value::Array(blocks.iter().cloned().collect())
            }
            None => serde_json::Value::Null,
        };
        tool_results.push((tool_name, tool_result.tool_use_id.clone(), response));
    }

    // Verify correlation worked
    assert!(
        pending_tools.is_empty(),
        "Tool should be removed after correlation"
    );
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].0, "Read");
    assert_eq!(tool_results[0].1, "toolu_123");
    assert_eq!(
        tool_results[0].2,
        serde_json::Value::String("hostname content".to_string())
    );
}

/// Test that unmatched tool results don't cause issues
#[test]
fn test_unmatched_tool_result() {
    let mut pending_tools: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut tool_results: Vec<(String, String)> = Vec::new();

    // ToolResult without a matching ToolUse
    let tool_result = ToolResultBlock {
        tool_use_id: "toolu_unknown".to_string(),
        content: Some(ToolResultContent::Text("some content".to_string())),
        is_error: None,
    };

    // Try to correlate - should not find a match
    if let Some((tool_name, _tool_input)) = pending_tools.remove(&tool_result.tool_use_id) {
        tool_results.push((tool_name, tool_result.tool_use_id.clone()));
    }

    // No results should be added
    assert!(tool_results.is_empty());
}

/// Test multiple tools in sequence
#[test]
fn test_multiple_tools_correlation() {
    let mut pending_tools: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut tool_results: Vec<String> = Vec::new();

    // Add multiple tool uses
    pending_tools.insert(
        "toolu_1".to_string(),
        ("Read".to_string(), serde_json::json!({})),
    );
    pending_tools.insert(
        "toolu_2".to_string(),
        ("Bash".to_string(), serde_json::json!({})),
    );
    pending_tools.insert(
        "toolu_3".to_string(),
        ("Grep".to_string(), serde_json::json!({})),
    );

    assert_eq!(pending_tools.len(), 3);

    // Process results in different order
    for tool_use_id in ["toolu_2", "toolu_1", "toolu_3"] {
        if let Some((tool_name, _)) = pending_tools.remove(tool_use_id) {
            tool_results.push(tool_name);
        }
    }

    assert!(pending_tools.is_empty());
    assert_eq!(tool_results, vec!["Bash", "Read", "Grep"]);
}

/// Test ContentBlock pattern matching
#[test]
fn test_content_block_matching() {
    let blocks: Vec<ContentBlock> = vec![
        ContentBlock::Text(claude_agent_sdk_rs::TextBlock {
            text: "Some text".to_string(),
        }),
        ContentBlock::ToolUse(ToolUseBlock {
            id: "tool_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/test"}),
        }),
        ContentBlock::ToolResult(ToolResultBlock {
            tool_use_id: "tool_1".to_string(),
            content: Some(ToolResultContent::Text("result".to_string())),
            is_error: None,
        }),
    ];

    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();

    for block in &blocks {
        match block {
            ContentBlock::ToolUse(tu) => {
                tool_uses.push(tu.name.clone());
            }
            ContentBlock::ToolResult(tr) => {
                tool_results.push(tr.tool_use_id.clone());
            }
            _ => {}
        }
    }

    assert_eq!(tool_uses, vec!["Read"]);
    assert_eq!(tool_results, vec!["tool_1"]);
}
