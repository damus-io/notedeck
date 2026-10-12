# A permission request that names its tool_use

claude-agent-sdk-rs's `ToolPermissionContext` drops the CLI's tool_use id, so
Dave guesses whose call a prompt is for by matching name and input.

- headway:dave/rude-food-bar — routing a subagent's prompt to its session meant a `PendingTool` parent map and a (name, input) match in `permission_subagent`, ambiguous when the session and a subagent make the same call at once
