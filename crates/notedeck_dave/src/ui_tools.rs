//! Dave's **UI tools**: the actions behind its normal-mode keys, by name.
//! Chrome's keymap binds the keys to them (`default_keymap.toml`, the `dave.*`
//! scopes), and its macros and (opted-in) agents can run them too (see
//! [`notedeck::App::ui_tools`]).
//!
//! Each tool parses into a [`KeyAction`] and runs through Dave's dispatch
//! (`Dave::dispatch_key_action`), the same one the Ctrl ladder uses. What an
//! action does to normal mode itself — `i` ending it, `h` pointing the motions
//! at the session list — is
//! [`NormalMode::after_action`], which [`begin_call`] runs.
//!
//! The tools are live only in normal mode, like the keys: this module answers
//! from state alone, without allocating, which tools are
//! [available](available) and which [key scopes](key_scopes) are active.

use claude_agent_sdk_rs::PermissionMode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use notedeck::{ToolArg, ToolArgType, ToolSpec};

use crate::ui::keybindings::{KeyAction, NormalMode, NormalView, Pane};
use crate::ui::SceneDir;

/// `j`/`k`/`gg`/`G`/`q` in the chat: move or drop the block cursor.
pub(crate) const BLOCK_CURSOR: &str = "dave_block_cursor";
/// `za`/`o`, `zo`, `zc`, `zr`, `zm`: fold the chat's blocks.
pub(crate) const BLOCK: &str = "dave_block";
/// `h` / `l`: point the motions at the session list or the chat.
pub(crate) const FOCUS_PANE: &str = "dave_focus_pane";
/// `j`/`k`/`gg`/`G` in the session list: switch sessions.
pub(crate) const SESSION_CURSOR: &str = "dave_session_cursor";
/// `h`/`j`/`k`/`l` on the scene's map: step to the nearest node that way.
pub(crate) const SCENE_CURSOR: &str = "dave_scene_cursor";
/// `i` / `a`: leave normal mode for the chat input.
pub(crate) const INSERT_MODE: &str = "dave_insert_mode";
/// `n`: start a new agent.
pub(crate) const NEW_AGENT: &str = "dave_new_agent";
/// `c`: clone the active agent.
pub(crate) const CLONE_AGENT: &str = "dave_clone_agent";
/// `r`: rename the active agent.
pub(crate) const RENAME_AGENT: &str = "dave_rename_agent";
/// `dd`: delete the active session.
pub(crate) const DELETE_SESSION: &str = "dave_delete_session";
/// `v`: flip between the scene and the classic view.
pub(crate) const TOGGLE_VIEW: &str = "dave_toggle_view";
/// `m m` / `m p` / `m e` / `m a`: set the permission mode.
pub(crate) const SET_PERMISSION_MODE: &str = "dave_set_permission_mode";
/// `e`: compose the message in an external editor.
pub(crate) const EXTERNAL_EDITOR: &str = "dave_external_editor";
/// `s`: open the issue the active session works.
pub(crate) const OPEN_ISSUE: &str = "dave_open_issue";
/// `S`: stop the active session's running turn.
pub(crate) const INTERRUPT: &str = "dave_interrupt";
/// `]q` / `[q`: step through the focus queue.
pub(crate) const FOCUS_QUEUE: &str = "dave_focus_queue";
/// `gd` / `gc`: show the active session's Details or Chat tab.
pub(crate) const SESSION_TAB: &str = "dave_session_tab";

/// The active session's issue, as data, for a macro step to hand on (its
/// `as`, or `{prev}`): `{"issue": "headway:<board>/<word-id>"}`. No key does
/// this, so it isn't a [`DaveUiTool`]: it reads, and doesn't act.
pub(crate) const SESSION_ISSUE: &str = "dave_session_issue";

/// The spec of [`SESSION_ISSUE`].
pub(crate) fn session_issue_spec() -> ToolSpec {
    ToolSpec::new(
        SESSION_ISSUE,
        "The issue the active session works, e.g. its headway card, as \
         {\"issue\": \"<ref>\"}. Changes nothing.",
        vec![],
    )
    .ui()
}

/// One of Dave's UI tools, by name. Its arguments pick the [`KeyAction`]
/// ([`parse`](Self::parse)); availability takes none ([`available`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaveUiTool {
    BlockCursor,
    Block,
    FocusPane,
    SessionCursor,
    SceneCursor,
    InsertMode,
    NewAgent,
    CloneAgent,
    RenameAgent,
    DeleteSession,
    ToggleView,
    SetPermissionMode,
    ExternalEditor,
    OpenIssue,
    Interrupt,
    FocusQueue,
    SessionTab,
}

/// Where `dave_block_cursor` takes the block cursor.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum BlockTo {
    Down,
    Up,
    First,
    Last,
    Clear,
}

/// The values of [`BlockTo`], as the spec advertises them.
const BLOCK_TOS: &[&str] = &["down", "up", "first", "last", "clear"];

/// What `dave_block` does to the chat's blocks.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum BlockOp {
    Toggle,
    Open,
    Close,
    ExpandAll,
    CollapseAll,
}

/// The values of [`BlockOp`].
const BLOCK_OPS: &[&str] = &["toggle", "open", "close", "expand_all", "collapse_all"];

/// Which pane `dave_focus_pane` points the motions at.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum PaneArg {
    Sessions,
    Chat,
}

/// The values of [`PaneArg`].
const PANES: &[&str] = &["sessions", "chat"];

/// Where `dave_session_cursor` moves in the session list.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SessionTo {
    Next,
    Prev,
    First,
    Last,
}

/// The values of [`SessionTo`].
const SESSION_TOS: &[&str] = &["next", "prev", "first", "last"];

/// Which way `dave_scene_cursor` steps on the map.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SceneTo {
    Left,
    Down,
    Up,
    Right,
}

/// The values of [`SceneTo`].
const SCENE_TOS: &[&str] = &["left", "down", "up", "right"];

/// Next or previous, for `dave_focus_queue`.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Dir {
    Next,
    Prev,
}

/// The values of [`Dir`].
const DIRS: &[&str] = &["next", "prev"];

/// Which of the session's tabs `dave_session_tab` shows.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Tab {
    Chat,
    Details,
}

/// The values of [`Tab`].
const TABS: &[&str] = &["chat", "details"];

/// The permission mode `dave_set_permission_mode` sets. Bypass isn't one:
/// Dave can't enter it mid-session, and it does no safety checking.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ModeArg {
    Manual,
    Plan,
    AcceptEdits,
    Auto,
}

/// The values of [`ModeArg`].
const MODES: &[&str] = &["manual", "plan", "accept_edits", "auto"];

/// `dave_block_cursor`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockCursorArgs {
    to: BlockTo,
}

/// `dave_block`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockArgs {
    op: BlockOp,
}

/// `dave_focus_pane`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneArgs {
    pane: PaneArg,
}

/// `dave_session_cursor`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionArgs {
    to: SessionTo,
}

/// `dave_scene_cursor`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneArgs {
    to: SceneTo,
}

/// `dave_focus_queue`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    dir: Dir,
}

/// `dave_session_tab`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabArgs {
    tab: Tab,
}

/// `dave_set_permission_mode`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeArgs {
    mode: ModeArg,
}

/// An argument-free tool's arguments: none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// A required enum argument named `name` over `values`.
fn enum_arg(name: &'static str, values: &'static [&'static str], doc: &'static str) -> ToolArg {
    ToolArg::new(name, ToolArgType::Enum(values.to_vec()), doc).required(true)
}

impl DaveUiTool {
    /// Every tool, in the order [`specs`](Self::specs) lists them.
    pub(crate) const ALL: [Self; 17] = [
        Self::BlockCursor,
        Self::Block,
        Self::FocusPane,
        Self::SessionCursor,
        Self::SceneCursor,
        Self::InsertMode,
        Self::NewAgent,
        Self::CloneAgent,
        Self::RenameAgent,
        Self::DeleteSession,
        Self::ToggleView,
        Self::SetPermissionMode,
        Self::ExternalEditor,
        Self::OpenIssue,
        Self::Interrupt,
        Self::FocusQueue,
        Self::SessionTab,
    ];

    /// The tool's name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::BlockCursor => BLOCK_CURSOR,
            Self::Block => BLOCK,
            Self::FocusPane => FOCUS_PANE,
            Self::SessionCursor => SESSION_CURSOR,
            Self::SceneCursor => SCENE_CURSOR,
            Self::InsertMode => INSERT_MODE,
            Self::NewAgent => NEW_AGENT,
            Self::CloneAgent => CLONE_AGENT,
            Self::RenameAgent => RENAME_AGENT,
            Self::DeleteSession => DELETE_SESSION,
            Self::ToggleView => TOGGLE_VIEW,
            Self::SetPermissionMode => SET_PERMISSION_MODE,
            Self::ExternalEditor => EXTERNAL_EDITOR,
            Self::OpenIssue => OPEN_ISSUE,
            Self::Interrupt => INTERRUPT,
            Self::FocusQueue => FOCUS_QUEUE,
            Self::SessionTab => SESSION_TAB,
        }
    }

    /// The tool called `name`, if Dave has one.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// The spec of every Dave UI tool, for [`notedeck::App::ui_tools`].
    pub(crate) fn specs() -> Vec<ToolSpec> {
        Self::ALL.into_iter().map(Self::spec).collect()
    }

    /// This tool's spec.
    fn spec(self) -> ToolSpec {
        let (description, args) = match self {
            Self::BlockCursor => (
                "Move the chat's block cursor through its collapsible blocks, or drop it \
                 (clear) so the transcript follows new output and normal mode ends.",
                vec![enum_arg("to", BLOCK_TOS, "Where the cursor goes.")],
            ),
            Self::Block => (
                "Fold the chat's blocks: toggle, open or close the one under the block \
                 cursor, or expand or collapse them all.",
                vec![enum_arg("op", BLOCK_OPS, "What to do.")],
            ),
            Self::FocusPane => (
                "Point normal mode's motions at the session list or the chat.",
                vec![enum_arg("pane", PANES, "The pane to move through.")],
            ),
            Self::SessionCursor => (
                "Switch sessions from the session list, without focusing the input.",
                vec![enum_arg("to", SESSION_TOS, "Which session to switch to.")],
            ),
            Self::SceneCursor => (
                "Step the scene view's selection to the nearest node on the map that way, \
                 switching to it without focusing the input. Right off the map's edge is \
                 the chat beside it.",
                vec![enum_arg("to", SCENE_TOS, "Which way to step.")],
            ),
            Self::InsertMode => (
                "Leave normal mode and focus the active session's chat input.",
                vec![],
            ),
            Self::NewAgent => ("Start a new agent session.", vec![]),
            Self::CloneAgent => (
                "Clone the active agent into a new session in the same directory.",
                vec![],
            ),
            Self::RenameAgent => ("Rename the active session.", vec![]),
            Self::DeleteSession => ("Delete the active session.", vec![]),
            Self::ToggleView => ("Flip between the scene view and the classic view.", vec![]),
            Self::SetPermissionMode => (
                "Set the active session's permission mode: manual (ask before each tool \
                 call), plan, accept_edits or auto (Claude Code's safety classifier \
                 decides). Bypass is never offered.",
                vec![enum_arg("mode", MODES, "The mode to set.")],
            ),
            Self::ExternalEditor => (
                "Compose the active session's message in an external editor.",
                vec![],
            ),
            Self::OpenIssue => (
                "Open the issue the active session works, e.g. its headway card.",
                vec![],
            ),
            Self::Interrupt => ("Stop the active session's running turn.", vec![]),
            Self::FocusQueue => (
                "Step to the next or previous session in the focus queue.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            ),
            Self::SessionTab => (
                "Show the active session's Chat tab or its Details tab (usage, background \
                 work, files, …).",
                vec![enum_arg("tab", TABS, "The tab to show.")],
            ),
        };
        ToolSpec::new(self.name(), description, args).ui()
    }

    /// Parse a call of the tool `name` with `args` (an object, or `null` for
    /// none) into the [`KeyAction`] its key produces. Errors on an unknown
    /// tool, arguments that aren't an object, a missing or unknown argument,
    /// or a value outside an argument's enum.
    pub(crate) fn parse(name: &str, args: &Value) -> Result<KeyAction, String> {
        let none = json!({});
        let args = if args.is_null() { &none } else { args };
        if !args.is_object() {
            return Err(format!("'{name}' arguments must be an object, not {args}"));
        }
        let Some(tool) = Self::from_name(name) else {
            return Err(format!("unknown tool '{name}'"));
        };
        use KeyAction as A;
        Ok(match tool {
            Self::BlockCursor => {
                let BlockCursorArgs { to } = parse_args(name, args)?;
                match to {
                    BlockTo::Down => A::BlockCursorDown,
                    BlockTo::Up => A::BlockCursorUp,
                    BlockTo::First => A::BlockCursorFirst,
                    BlockTo::Last => A::BlockCursorLast,
                    BlockTo::Clear => A::BlockCursorClear,
                }
            }
            Self::Block => {
                let BlockArgs { op } = parse_args(name, args)?;
                match op {
                    BlockOp::Toggle => A::BlockToggle,
                    BlockOp::Open => A::BlockOpen,
                    BlockOp::Close => A::BlockClose,
                    BlockOp::ExpandAll => A::BlockExpandAll,
                    BlockOp::CollapseAll => A::BlockCollapseAll,
                }
            }
            Self::FocusPane => {
                let PaneArgs { pane } = parse_args(name, args)?;
                match pane {
                    PaneArg::Sessions => A::FocusSessionsPane,
                    PaneArg::Chat => A::FocusChatPane,
                }
            }
            Self::SessionCursor => {
                let SessionArgs { to } = parse_args(name, args)?;
                match to {
                    SessionTo::Next => A::SessionPaneNext,
                    SessionTo::Prev => A::SessionPanePrev,
                    SessionTo::First => A::SessionPaneFirst,
                    SessionTo::Last => A::SessionPaneLast,
                }
            }
            Self::SceneCursor => {
                let SceneArgs { to } = parse_args(name, args)?;
                A::SceneStep(match to {
                    SceneTo::Left => SceneDir::Left,
                    SceneTo::Down => SceneDir::Down,
                    SceneTo::Up => SceneDir::Up,
                    SceneTo::Right => SceneDir::Right,
                })
            }
            Self::FocusQueue => {
                let QueueArgs { dir } = parse_args(name, args)?;
                match dir {
                    Dir::Next => A::FocusQueueNext,
                    Dir::Prev => A::FocusQueuePrev,
                }
            }
            Self::SetPermissionMode => {
                let ModeArgs { mode } = parse_args(name, args)?;
                A::SetPermissionMode(match mode {
                    ModeArg::Manual => PermissionMode::Default,
                    ModeArg::Plan => PermissionMode::Plan,
                    ModeArg::AcceptEdits => PermissionMode::AcceptEdits,
                    ModeArg::Auto => PermissionMode::Auto,
                })
            }
            Self::SessionTab => {
                let TabArgs { tab } = parse_args(name, args)?;
                match tab {
                    Tab::Chat => A::ShowChat,
                    Tab::Details => A::ShowDetails,
                }
            }
            Self::InsertMode => no_args(name, args, A::InsertMode)?,
            Self::NewAgent => no_args(name, args, A::NewAgent)?,
            Self::CloneAgent => no_args(name, args, A::CloneAgent)?,
            Self::RenameAgent => no_args(name, args, A::RenameAgent)?,
            Self::DeleteSession => no_args(name, args, A::DeleteActiveSession)?,
            Self::ToggleView => no_args(name, args, A::ToggleView)?,
            Self::ExternalEditor => no_args(name, args, A::OpenExternalEditor)?,
            Self::OpenIssue => no_args(name, args, A::OpenIssue)?,
            Self::Interrupt => no_args(name, args, A::Interrupt)?,
        })
    }
}

/// Deserialize `args` for the tool `name`.
fn parse_args<T: DeserializeOwned>(name: &str, args: &Value) -> Result<T, String> {
    T::deserialize(args).map_err(|e| format!("failed to parse '{name}' arguments: {e}"))
}

/// `action`, once `args` is checked to hold nothing.
fn no_args(name: &str, args: &Value, action: KeyAction) -> Result<KeyAction, String> {
    let NoArgs {} = parse_args(name, args)?;
    Ok(action)
}

/// Whether `tool` does anything now. `normal` is normal mode as Dave sees it
/// this frame, or `None` outside it (insert mode, an overlay open, renaming),
/// where no tool is available. In it, the block tools need the chat pane and
/// the session cursor the session list, and the scene cursor the scene's map
/// standing in for it; `dave_open_issue` needs an issue,
/// `dave_interrupt` a running turn, and the agentic-only tools an agentic
/// session. A key bound to a tool that isn't available falls through to Dave,
/// whose normal mode swallows it.
///
/// `dave_focus_pane` is available whatever its pane: availability takes no
/// arguments, and pointing at a session list that isn't shown does nothing,
/// as `h` does.
pub(crate) fn available(tool: DaveUiTool, normal: Option<NormalView>) -> bool {
    let Some(view) = normal else {
        return false;
    };
    match tool {
        DaveUiTool::BlockCursor | DaveUiTool::Block => view.pane == Pane::Chat,
        DaveUiTool::SessionCursor => view.pane == Pane::Sessions,
        DaveUiTool::SceneCursor => view.pane == Pane::Sessions && view.scene,
        DaveUiTool::OpenIssue => view.has_issue,
        DaveUiTool::Interrupt => view.interruptible,
        DaveUiTool::CloneAgent
        | DaveUiTool::ToggleView
        | DaveUiTool::SetPermissionMode
        | DaveUiTool::FocusQueue => view.agentic,
        DaveUiTool::FocusPane
        | DaveUiTool::InsertMode
        | DaveUiTool::NewAgent
        | DaveUiTool::RenameAgent
        | DaveUiTool::DeleteSession
        | DaveUiTool::ExternalEditor
        | DaveUiTool::SessionTab => true,
    }
}

/// The first half of calling the UI tool `name` with `args` in normal mode
/// `view`: check it, and change `mode` as the action does
/// ([`NormalMode::after_action`]). Returns the action left for Dave to
/// dispatch, or `None` when the call does nothing (`dave_focus_pane` at a
/// session list that isn't shown, as `h` there). An unknown tool, bad
/// arguments or a tool that isn't [available] is an error, and leaves `mode`
/// alone.
pub(crate) fn begin_call(
    mode: &mut NormalMode,
    view: Option<NormalView>,
    name: &str,
    args: &Value,
) -> Result<Option<KeyAction>, String> {
    let action = DaveUiTool::parse(name, args)?;
    let Some(tool) = DaveUiTool::from_name(name) else {
        return Err(format!("unknown tool '{name}'"));
    };
    if !available(tool, view) {
        return Err(format!("'{name}' is not available now"));
    }
    if action == KeyAction::FocusSessionsPane && !mode.sessions_shown() {
        return Ok(None);
    }
    mode.after_action(&action);
    Ok(Some(action))
}

/// Normal mode in the session list.
const SESSIONS_SCOPES: &[&str] = &["dave.sessions", "dave.normal", "dave"];
/// Normal mode on the scene's map: its steps, ahead of the session list's
/// keys (`gg`, `G`, Enter), which it stands in for.
const SCENE_SCOPES: &[&str] = &["dave.scene", "dave.sessions", "dave.normal", "dave"];
/// Normal mode in the chat.
const CHAT_SCOPES: &[&str] = &["dave.chat", "dave.normal", "dave"];

/// The key scopes active for `normal` (see [`available`] for what `None`
/// covers): the pane's, then normal mode's, then Dave's. None outside normal
/// mode, where keys are typing.
pub(crate) fn key_scopes(normal: Option<NormalView>) -> &'static [&'static str] {
    let Some(view) = normal else {
        return &[];
    };
    match view.pane {
        Pane::Sessions if view.scene => SCENE_SCOPES,
        Pane::Sessions => SESSIONS_SCOPES,
        Pane::Chat => CHAT_SCOPES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Normal mode in `pane`, with everything on offer.
    fn view(pane: Pane) -> NormalView {
        NormalView {
            pane,
            sessions_shown: true,
            scene: false,
            agentic: true,
            interruptible: true,
            has_issue: true,
        }
    }

    #[test]
    fn every_tool_round_trips_its_name_and_is_a_ui_tool() {
        let specs = DaveUiTool::specs();
        assert_eq!(specs.len(), DaveUiTool::ALL.len());
        for (tool, spec) in DaveUiTool::ALL.into_iter().zip(&specs) {
            assert_eq!(DaveUiTool::from_name(tool.name()), Some(tool));
            assert_eq!(spec.name(), tool.name());
            assert_eq!(spec.kind(), notedeck::ToolKind::Ui);
            assert!(spec.name().starts_with("dave_"), "{}", spec.name());
        }
    }

    /// Each tool's arguments name the chord key's action.
    #[test]
    fn tools_parse_into_their_keys_actions() {
        use KeyAction as A;
        let cases = [
            (BLOCK_CURSOR, json!({"to": "down"}), A::BlockCursorDown),
            (BLOCK_CURSOR, json!({"to": "up"}), A::BlockCursorUp),
            (BLOCK_CURSOR, json!({"to": "first"}), A::BlockCursorFirst),
            (BLOCK_CURSOR, json!({"to": "last"}), A::BlockCursorLast),
            (BLOCK_CURSOR, json!({"to": "clear"}), A::BlockCursorClear),
            (BLOCK, json!({"op": "toggle"}), A::BlockToggle),
            (BLOCK, json!({"op": "open"}), A::BlockOpen),
            (BLOCK, json!({"op": "close"}), A::BlockClose),
            (BLOCK, json!({"op": "expand_all"}), A::BlockExpandAll),
            (BLOCK, json!({"op": "collapse_all"}), A::BlockCollapseAll),
            (
                FOCUS_PANE,
                json!({"pane": "sessions"}),
                A::FocusSessionsPane,
            ),
            (FOCUS_PANE, json!({"pane": "chat"}), A::FocusChatPane),
            (SESSION_CURSOR, json!({"to": "next"}), A::SessionPaneNext),
            (SESSION_CURSOR, json!({"to": "prev"}), A::SessionPanePrev),
            (SESSION_CURSOR, json!({"to": "first"}), A::SessionPaneFirst),
            (SESSION_CURSOR, json!({"to": "last"}), A::SessionPaneLast),
            (
                SCENE_CURSOR,
                json!({"to": "left"}),
                A::SceneStep(SceneDir::Left),
            ),
            (
                SCENE_CURSOR,
                json!({"to": "down"}),
                A::SceneStep(SceneDir::Down),
            ),
            (
                SCENE_CURSOR,
                json!({"to": "up"}),
                A::SceneStep(SceneDir::Up),
            ),
            (
                SCENE_CURSOR,
                json!({"to": "right"}),
                A::SceneStep(SceneDir::Right),
            ),
            (FOCUS_QUEUE, json!({"dir": "next"}), A::FocusQueueNext),
            (FOCUS_QUEUE, json!({"dir": "prev"}), A::FocusQueuePrev),
            (INSERT_MODE, Value::Null, A::InsertMode),
            (NEW_AGENT, json!({}), A::NewAgent),
            (CLONE_AGENT, json!({}), A::CloneAgent),
            (RENAME_AGENT, json!({}), A::RenameAgent),
            (DELETE_SESSION, json!({}), A::DeleteActiveSession),
            (TOGGLE_VIEW, json!({}), A::ToggleView),
            (
                SET_PERMISSION_MODE,
                json!({"mode": "manual"}),
                A::SetPermissionMode(PermissionMode::Default),
            ),
            (
                SET_PERMISSION_MODE,
                json!({"mode": "plan"}),
                A::SetPermissionMode(PermissionMode::Plan),
            ),
            (
                SET_PERMISSION_MODE,
                json!({"mode": "accept_edits"}),
                A::SetPermissionMode(PermissionMode::AcceptEdits),
            ),
            (
                SET_PERMISSION_MODE,
                json!({"mode": "auto"}),
                A::SetPermissionMode(PermissionMode::Auto),
            ),
            (EXTERNAL_EDITOR, json!({}), A::OpenExternalEditor),
            (OPEN_ISSUE, json!({}), A::OpenIssue),
            (INTERRUPT, json!({}), A::Interrupt),
            (SESSION_TAB, json!({"tab": "details"}), A::ShowDetails),
            (SESSION_TAB, json!({"tab": "chat"}), A::ShowChat),
        ];
        for (name, args, want) in cases {
            assert_eq!(DaveUiTool::parse(name, &args), Ok(want), "{name} {args}");
        }
    }

    #[test]
    fn bad_calls_are_errors() {
        for (name, args) in [
            ("dave_nope", json!({})),
            (BLOCK_CURSOR, json!({})),
            (BLOCK_CURSOR, json!({"to": "sideways"})),
            (BLOCK, json!({"op": "toggle", "extra": 1})),
            (SET_PERMISSION_MODE, json!({})),
            (SET_PERMISSION_MODE, json!({"mode": "bypass_permissions"})),
            (NEW_AGENT, json!({"now": true})),
            (INSERT_MODE, json!([])),
        ] {
            assert!(DaveUiTool::parse(name, &args).is_err(), "{name} {args}");
        }
    }

    /// The truth table: nothing outside normal mode; the pane tools by pane;
    /// the rest by what the active session offers.
    #[test]
    fn availability_follows_the_mode_the_pane_and_the_session() {
        for tool in DaveUiTool::ALL {
            assert!(!available(tool, None), "{tool:?} outside normal mode");
        }

        for tool in DaveUiTool::ALL {
            let in_chat = available(tool, Some(view(Pane::Chat)));
            let in_sessions = available(tool, Some(view(Pane::Sessions)));
            match tool {
                DaveUiTool::BlockCursor | DaveUiTool::Block => {
                    assert!(in_chat && !in_sessions, "{tool:?}")
                }
                DaveUiTool::SessionCursor => assert!(!in_chat && in_sessions, "{tool:?}"),
                DaveUiTool::SceneCursor => assert!(!in_chat && !in_sessions, "{tool:?}"),
                _ => assert!(in_chat && in_sessions, "{tool:?}"),
            }
        }

        // The scene's map stands in for the list: its steps join the list's
        // cursor there, and stay off in the chat beside it.
        let map = NormalView {
            scene: true,
            ..view(Pane::Sessions)
        };
        let scene_chat = NormalView {
            scene: true,
            ..view(Pane::Chat)
        };
        assert!(available(DaveUiTool::SceneCursor, Some(map)));
        assert!(available(DaveUiTool::SessionCursor, Some(map)));
        assert!(!available(DaveUiTool::SceneCursor, Some(scene_chat)));

        let bare = NormalView {
            agentic: false,
            interruptible: false,
            has_issue: false,
            sessions_shown: false,
            scene: false,
            ..view(Pane::Chat)
        };
        for tool in DaveUiTool::ALL {
            let off = matches!(
                tool,
                DaveUiTool::OpenIssue
                    | DaveUiTool::Interrupt
                    | DaveUiTool::CloneAgent
                    | DaveUiTool::ToggleView
                    | DaveUiTool::SetPermissionMode
                    | DaveUiTool::FocusQueue
            ) || matches!(tool, DaveUiTool::SessionCursor | DaveUiTool::SceneCursor);
            assert_eq!(available(tool, Some(bare)), !off, "{tool:?}");
        }
    }

    #[test]
    fn scopes_follow_the_mode_and_the_pane() {
        assert!(key_scopes(None).is_empty());
        assert_eq!(
            key_scopes(Some(view(Pane::Chat))),
            ["dave.chat", "dave.normal", "dave"]
        );
        assert_eq!(
            key_scopes(Some(view(Pane::Sessions))),
            ["dave.sessions", "dave.normal", "dave"]
        );
        let map = NormalView {
            scene: true,
            ..view(Pane::Sessions)
        };
        assert_eq!(
            key_scopes(Some(map)),
            ["dave.scene", "dave.sessions", "dave.normal", "dave"]
        );
    }
}

/// The tools on a real [`Dave`](crate::Dave), called through
/// [`App::call_ui_tool`](notedeck::App::call_ui_tool) with a host's
/// [`AppContext`](notedeck::AppContext), the way chrome calls them.
#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::tests::test_dave;
    use crate::{Dave, DaveOverlay, SessionId};
    use notedeck::{App, AppAction, DataPath, Notedeck, ToolOutcome};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A test [`Dave`] with two agentic sessions, the first active, in normal
    /// mode in the chat, beside a host to hand its tools an `AppContext`.
    struct Fixture {
        _dirs: [TempDir; 2],
        egui: egui::Context,
        notedeck: Notedeck,
        dave: Dave,
        sessions: [SessionId; 2],
    }

    impl Fixture {
        fn new() -> Self {
            let host_dir = TempDir::new().unwrap();
            let dave_dir = TempDir::new().unwrap();
            let egui = egui::Context::default();
            let args = ["notedeck-test".to_owned(), "--testrunner".to_owned()];
            let notedeck = Notedeck::init(&egui, host_dir.path(), &args);
            let mut dave = test_dave(&DataPath::new(dave_dir.path()));
            let sessions = ["/tmp/tool-a", "/tmp/tool-b"].map(|cwd| {
                dave.session_manager.new_session(
                    PathBuf::from(cwd),
                    AiMode::Agentic,
                    BackendType::Remote,
                )
            });
            dave.session_manager.switch_to(sessions[0]);
            for id in sessions {
                dave.session_manager.get_mut(id).unwrap().focus_requested = false;
            }
            // An empty agentic Dave opens on the directory picker; the
            // sessions above are what it would have picked.
            dave.active_overlay = DaveOverlay::None;
            dave.normal_mode.open(&egui);
            Fixture {
                _dirs: [host_dir, dave_dir],
                egui,
                notedeck,
                dave,
                sessions,
            }
        }

        /// Call the tool `name` as chrome does.
        fn call(&mut self, name: &str, args: Value) -> ToolOutcome {
            let mut ctx = self.notedeck.app_context();
            self.dave.call_ui_tool(&mut ctx, name, &args)
        }

        fn focus_requested(&self, id: SessionId) -> bool {
            self.dave.session_manager.get(id).unwrap().focus_requested
        }
    }

    fn is_null(outcome: &ToolOutcome) -> bool {
        matches!(outcome, ToolOutcome::Data(Value::Null))
    }

    /// The session-list `j`, as a tool: it switches sessions and leaves the
    /// keyboard with normal mode; `dave_insert_mode` then hands the new
    /// session's input focus, as `i` does.
    #[test]
    fn the_session_cursor_switches_without_grabbing_focus() {
        let mut f = Fixture::new();
        let [first, second] = f.sessions;
        assert!(
            matches!(
                f.call(SESSION_CURSOR, json!({"to": "next"})),
                ToolOutcome::Error(_)
            ),
            "the session cursor is the session list's"
        );

        // `h` reaches the list only once a frame has seen it on screen; the
        // tool and the key share what it then does.
        f.dave
            .normal_mode
            .after_action(&KeyAction::FocusSessionsPane);
        assert_eq!(f.dave.key_scopes(), SESSIONS_SCOPES);
        assert!(f.dave.ui_tool_available(SESSION_CURSOR));
        assert!(!f.dave.ui_tool_available(BLOCK_CURSOR));

        assert!(is_null(&f.call(SESSION_CURSOR, json!({"to": "next"}))));
        assert_eq!(f.dave.session_manager.active_id(), Some(second));
        assert!(!f.focus_requested(first));
        assert!(!f.focus_requested(second), "normal mode holds the keyboard");

        assert!(is_null(&f.call(INSERT_MODE, Value::Null)));
        assert!(!f.dave.normal_mode.is_normal());
        assert!(
            f.focus_requested(second),
            "the new session's input takes it"
        );
        assert!(f.dave.key_scopes().is_empty());
        for tool in DaveUiTool::ALL {
            assert!(!f.dave.ui_tool_available(tool.name()), "{tool:?}");
        }
        assert!(matches!(
            f.call(NEW_AGENT, Value::Null),
            ToolOutcome::Error(_)
        ));
    }

    /// `s`, as a tool: chrome gets the session's issue to open, and without
    /// one the tool isn't offered.
    #[test]
    fn open_issue_hands_chrome_the_sessions_issue() {
        let mut f = Fixture::new();
        assert!(!f.dave.ui_tool_available(OPEN_ISSUE));
        assert!(matches!(
            f.call(OPEN_ISSUE, Value::Null),
            ToolOutcome::Error(_)
        ));

        let [first, _] = f.sessions;
        f.dave
            .session_manager
            .get_mut(first)
            .unwrap()
            .details
            .issue_url = Some("headway:notedeck/use-release-oyster".to_owned());
        assert!(f.dave.ui_tool_available(OPEN_ISSUE));
        match f.call(OPEN_ISSUE, Value::Null) {
            ToolOutcome::Intent(AppAction::Open(open)) => {
                assert_eq!(open.reference, "headway:notedeck/use-release-oyster")
            }
            _ => panic!("expected an open intent"),
        }
        assert!(f.dave.normal_mode.is_normal(), "s keeps normal mode on");
    }

    /// `dave_session_issue` answers with the active session's issue, for a
    /// macro step to hand on, and changes nothing; without an issue it isn't
    /// offered. It needs no window and no normal mode.
    #[test]
    fn session_issue_answers_with_the_issue_and_changes_nothing() {
        let mut f = Fixture::new();
        assert!(!f.dave.ui_tool_available(SESSION_ISSUE));
        assert!(matches!(
            f.call(SESSION_ISSUE, Value::Null),
            ToolOutcome::Error(_)
        ));

        let [first, _] = f.sessions;
        f.dave
            .session_manager
            .get_mut(first)
            .unwrap()
            .details
            .issue_url = Some("headway:notedeck/use-release-oyster".to_owned());
        f.dave.normal_mode = NormalMode::default();
        assert!(!f.dave.normal_mode.is_normal());
        assert!(f.dave.ui_tool_available(SESSION_ISSUE), "in any mode");
        match f.call(SESSION_ISSUE, Value::Null) {
            ToolOutcome::Data(data) => {
                assert_eq!(
                    data,
                    json!({"issue": "headway:notedeck/use-release-oyster"})
                )
            }
            _ => panic!("expected the issue as data"),
        }
        assert_eq!(f.dave.session_manager.active_id(), Some(first));
        assert!(f
            .dave
            .ui_tools()
            .iter()
            .any(|spec| spec.name() == SESSION_ISSUE));
    }

    /// `dd`, `v`, `n`, `q` and `l`, as tools, do what their keys do.
    #[test]
    fn session_and_view_tools_do_what_their_keys_do() {
        let mut f = Fixture::new();
        let [first, second] = f.sessions;

        assert!(is_null(&f.call(DELETE_SESSION, Value::Null)));
        assert!(f.dave.session_manager.get(first).is_none());
        assert_eq!(f.dave.session_manager.active_id(), Some(second));
        assert!(f.dave.normal_mode.is_normal(), "dd keeps normal mode on");

        let scene = f.dave.show_scene;
        assert!(is_null(&f.call(TOGGLE_VIEW, Value::Null)));
        assert_eq!(f.dave.show_scene, !scene);

        assert!(is_null(&f.call(FOCUS_PANE, json!({"pane": "chat"}))));
        assert_eq!(f.dave.key_scopes(), CHAT_SCOPES);

        assert!(is_null(&f.call(BLOCK_CURSOR, json!({"to": "clear"}))));
        assert!(!f.dave.normal_mode.is_normal(), "q ends normal mode");
        assert!(f.focus_requested(second));

        f.dave.normal_mode.open(&f.egui);
        f.dave
            .session_manager
            .get_mut(second)
            .unwrap()
            .focus_requested = false;
        let _ = f.call(NEW_AGENT, Value::Null);
        assert!(!f.dave.normal_mode.is_normal(), "n leaves normal mode");
        assert!(
            !f.focus_requested(second),
            "n leaves focus to the picker it opens"
        );
    }

    /// `dd` in the scene view takes the map's whole selection when a box
    /// select made it several, as Delete does: the map has no Delete of its
    /// own to run beside it.
    #[test]
    fn dd_in_the_scene_deletes_the_maps_selection() {
        let mut f = Fixture::new();
        let third = f.dave.session_manager.new_session(
            PathBuf::from("/tmp/tool-c"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        let [first, second] = f.sessions;
        f.dave.show_scene = true;
        f.dave.scene.select(first);
        f.dave.scene.add_to_selection(second);
        f.dave.session_manager.switch_to(first);

        assert!(is_null(&f.call(DELETE_SESSION, Value::Null)));
        assert!(f.dave.session_manager.get(first).is_none());
        assert!(f.dave.session_manager.get(second).is_none());
        assert_eq!(f.dave.session_manager.active_id(), Some(third));
        assert_eq!(f.dave.scene.selected, [third], "the selection follows");
    }

    /// `dd` on a session the list has folded away (the scene's map still
    /// shows it) has no row below to move to: the scene's selection follows
    /// the session the manager falls back to, not the deleted node.
    #[test]
    fn dd_in_the_scene_without_a_neighbor_moves_the_selection_on() {
        let mut f = Fixture::new();
        let [first, second] = f.sessions;
        let host = f
            .dave
            .session_manager
            .get(first)
            .unwrap()
            .details
            .hostname
            .clone();
        f.dave.collapse_state.toggle_host(&host);
        assert!(
            f.dave
                .session_manager
                .visual_order(&f.dave.collapse_state)
                .is_empty(),
            "folded away"
        );
        f.dave.show_scene = true;
        f.dave.scene.select(first);

        assert!(is_null(&f.call(DELETE_SESSION, Value::Null)));
        assert!(f.dave.session_manager.get(first).is_none());
        assert_eq!(f.dave.session_manager.active_id(), Some(second));
        assert_eq!(f.dave.scene.selected, [second]);
    }

    /// An overlay over the chat takes Dave's keys, so it takes its tools too.
    #[test]
    fn an_overlay_offers_no_scopes_or_tools() {
        let mut f = Fixture::new();
        assert_eq!(f.dave.key_scopes(), CHAT_SCOPES);
        f.dave.active_overlay = DaveOverlay::Settings;
        assert!(f.dave.key_scopes().is_empty());
        assert!(!f.dave.ui_tool_available(INSERT_MODE));
    }
}

/// Chrome's part of a Dave frame, for Dave's own key tests: chrome's key
/// engine on the shipped `default_keymap.toml`, resolving the frame's press
/// against normal mode's [key scopes](key_scopes) ahead of Dave, as the app
/// does, and the tool it's bound to begun on normal mode as
/// `Dave::run_ui_tool` begins it ([`begin_call`]). Chrome's own tools (`?`'s
/// strip) aren't offered, so their keys reach Dave.
#[cfg(test)]
pub(crate) mod chrome_keys {
    use super::*;
    use crate::ui::keybindings::ActiveOffers;
    use notedeck::ToolInvocation;
    use notedeck_chrome::keymap::{KeyDispatch, KeyEngine, Scopes};

    /// The engine, and every call it made.
    pub(crate) struct ChromeKeys {
        engine: KeyEngine,
        /// The tool calls the keys made, oldest first.
        pub(crate) calls: Vec<ToolInvocation>,
    }

    impl ChromeKeys {
        pub(crate) fn new() -> Self {
            let engine = KeyEngine::with_default_keymap();
            assert!(engine.keymap().warnings().is_empty());
            Self {
                engine,
                calls: Vec::new(),
            }
        }

        /// The engine, for its keymap and pending chord.
        pub(crate) fn engine(&self) -> &KeyEngine {
            &self.engine
        }

        /// Resolve this frame's press against `mode` with the active
        /// session's `offers` (no scopes under an overlay, as
        /// `Dave::normal_view` says), and begin the tool it's bound to.
        /// Returns the action left for Dave to dispatch. A binding whose
        /// call fails is a broken keymap, so it panics.
        pub(crate) fn frame(
            &mut self,
            ctx: &egui::Context,
            mode: &mut NormalMode,
            offers: ActiveOffers,
            overlay_open: bool,
        ) -> Option<KeyAction> {
            let view = if overlay_open {
                None
            } else {
                mode.view(offers)
            };
            let available =
                |name: &str| DaveUiTool::from_name(name).is_some_and(|tool| available(tool, view));
            let Some(KeyDispatch::Tool(call)) =
                self.engine
                    .resolve(ctx, Scopes::new(key_scopes(view)), available)
            else {
                return None;
            };
            let action = begin_call(mode, view, &call.name, &call.args)
                .unwrap_or_else(|err| panic!("keymap binds {} {}: {err}", call.name, call.args));
            self.calls.push(call);
            action
        }
    }
}
