/// Session info parsing utilities for Claude backend responses.
use crate::messages::SessionInfo;

/// Parse a System message into SessionInfo
pub fn parse_session_info(system_msg: &claude_agent_sdk_rs::SystemMessage) -> SessionInfo {
    let data = &system_msg.data;

    // Extract slash_commands from data
    let slash_commands = data
        .get("slash_commands")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract agents from data
    let agents = data
        .get("agents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract CLI version
    let cli_version = data
        .get("claude_code_version")
        .and_then(|v| v.as_str())
        .map(String::from);

    SessionInfo {
        tools: system_msg.tools.clone().unwrap_or_default(),
        model: system_msg.model.clone(),
        permission_mode: system_msg.permission_mode.clone(),
        slash_commands,
        agents,
        cli_version,
        cwd: system_msg.cwd.clone(),
        claude_session_id: system_msg.session_id.clone(),
    }
}
