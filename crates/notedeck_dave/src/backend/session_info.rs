/// Session info parsing utilities for Claude backend responses.
use crate::messages::{McpServerInfo, McpServerStatus, PluginError, PluginInfo, SessionInfo};
use serde_json::Value;

/// Parse a System message into SessionInfo.
///
/// Lenient: a field the CLI didn't send, or sent in a shape this build doesn't
/// know, comes out empty rather than failing the whole message. Older CLIs
/// omit most of the fields past `agents`.
pub fn parse_session_info(system_msg: &claude_agent_sdk_rs::SystemMessage) -> SessionInfo {
    let data = &system_msg.data;

    SessionInfo {
        tools: system_msg.tools.clone().unwrap_or_default(),
        model: system_msg.model.clone(),
        permission_mode: system_msg.permission_mode.clone(),
        slash_commands: strings(data, "slash_commands"),
        agents: strings(data, "agents"),
        cli_version: string(data, "claude_code_version"),
        cwd: system_msg.cwd.clone(),
        claude_session_id: system_msg.session_id.clone(),
        // A typed field on the SDK's message, so serde never leaves it in `data`.
        mcp_servers: system_msg
            .mcp_servers
            .iter()
            .flatten()
            .filter_map(|server| {
                Some(McpServerInfo {
                    name: string(server, "name")?,
                    status: server
                        .get("status")
                        .and_then(Value::as_str)
                        .map(McpServerStatus::from_wire)
                        .unwrap_or(McpServerStatus::Unknown),
                    source: string(server, "source"),
                })
            })
            .collect(),
        skills: strings(data, "skills"),
        plugins: objects(data, "plugins")
            .filter_map(|plugin| {
                Some(PluginInfo {
                    name: string(plugin, "name")?,
                    path: string(plugin, "path"),
                    version: string(plugin, "version"),
                })
            })
            .collect(),
        plugin_errors: objects(data, "plugin_errors")
            .map(|err| PluginError {
                plugin: string(err, "plugin").unwrap_or_default(),
                kind: string(err, "type").unwrap_or_default(),
                message: string(err, "message").unwrap_or_default(),
            })
            .collect(),
        output_style: string(data, "output_style"),
        betas: strings(data, "betas"),
        api_key_source: string(data, "apiKeySource"),
    }
}

/// `data[key]` as a string, if it is one.
fn string(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(String::from)
}

/// The string entries of the array `data[key]`; empty when it is missing.
fn strings(data: &Value, key: &str) -> Vec<String> {
    objects(data, key)
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

/// The entries of the array `data[key]`; none when it is missing.
fn objects<'a>(data: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    data.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `system/init` message deserialized the way the SDK does it, so the
    /// typed fields (`tools`, `mcp_servers`, …) are lifted out of `data`.
    fn system_init(fields: Value) -> claude_agent_sdk_rs::SystemMessage {
        let mut msg = serde_json::json!({ "subtype": "init" });
        msg.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value(msg).expect("system message")
    }

    /// Every field a Claude Code 2.1.288 init carries, in its wire shape.
    #[test]
    fn parses_every_init_field() {
        let msg = system_init(serde_json::json!({
            "cwd": "/home/jb55/dev/notedeck",
            "session_id": "0b8c1a2e-cli",
            "tools": ["Bash", "Read", "Edit"],
            "model": "claude-opus-5-5",
            "permissionMode": "default",
            "slash_commands": ["compact", "review"],
            "agents": ["general-purpose", "Explore"],
            "claude_code_version": "2.1.288",
            "apiKeySource": "none",
            "betas": ["context-1m-2025-08-07"],
            "output_style": "default",
            "skills": ["headway", "autowork"],
            "mcp_servers": [
                { "name": "linear-server", "status": "connected", "source": "user" },
                { "name": "beads", "status": "failed" },
                { "name": "notion", "status": "needs-auth", "source": "project" },
                { "name": "future", "status": "warming-up" },
            ],
            "plugins": [
                { "name": "notedeck", "path": "/home/jb55/.claude/plugins/notedeck", "version": "0.3.1" },
                { "name": "bare", "path": "/tmp/bare" },
            ],
            "plugin_errors": [
                { "plugin": "broken", "type": "manifest", "message": "missing name", "path": "/tmp/broken" },
            ],
        }));

        let info = parse_session_info(&msg);

        assert_eq!(info.cli_version.as_deref(), Some("2.1.288"));
        assert_eq!(info.tools.len(), 3);
        assert_eq!(info.agents, ["general-purpose", "Explore"]);
        assert_eq!(info.skills, ["headway", "autowork"]);
        assert_eq!(info.output_style.as_deref(), Some("default"));
        assert_eq!(info.betas, ["context-1m-2025-08-07"]);
        assert_eq!(info.api_key_source.as_deref(), Some("none"));
        assert_eq!(
            info.mcp_servers,
            [
                McpServerInfo {
                    name: "linear-server".into(),
                    status: McpServerStatus::Connected,
                    source: Some("user".into()),
                },
                McpServerInfo {
                    name: "beads".into(),
                    status: McpServerStatus::Failed,
                    source: None,
                },
                McpServerInfo {
                    name: "notion".into(),
                    status: McpServerStatus::NeedsAuth,
                    source: Some("project".into()),
                },
                McpServerInfo {
                    name: "future".into(),
                    status: McpServerStatus::Unknown,
                    source: None,
                },
            ]
        );
        assert_eq!(
            info.plugins,
            [
                PluginInfo {
                    name: "notedeck".into(),
                    path: Some("/home/jb55/.claude/plugins/notedeck".into()),
                    version: Some("0.3.1".into()),
                },
                PluginInfo {
                    name: "bare".into(),
                    path: Some("/tmp/bare".into()),
                    version: None,
                },
            ]
        );
        assert_eq!(
            info.plugin_errors,
            [PluginError {
                plugin: "broken".into(),
                kind: "manifest".into(),
                message: "missing name".into(),
            }]
        );
    }

    /// An init from a CLI that predates every inspector field still parses,
    /// with the new fields empty.
    #[test]
    fn missing_init_fields_are_empty() {
        let msg = system_init(serde_json::json!({
            "session_id": "old-cli",
            "tools": ["Bash"],
            "skills": "not an array",
        }));

        let info = parse_session_info(&msg);

        assert_eq!(info.claude_session_id.as_deref(), Some("old-cli"));
        assert!(info.mcp_servers.is_empty());
        assert!(info.skills.is_empty());
        assert!(info.plugins.is_empty());
        assert!(info.plugin_errors.is_empty());
        assert!(info.betas.is_empty());
        assert_eq!(info.output_style, None);
        assert_eq!(info.api_key_source, None);
        assert_eq!(info.cli_version, None);
    }
}
