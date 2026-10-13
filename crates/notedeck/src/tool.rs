//! Browser-level, statically-typed agent tools.
//!
//! An "agent tool" is a capability an AI backend can invoke — most importantly
//! a full-text nostrdb query. These are *browser*-owned: they operate on shared
//! state the host holds (the db, caches, accounts), not on any one app's private
//! state, so notedeck models them as a single statically-typed [`ToolCall`]
//! enum rather than a bag of dynamically-dispatched handlers. A dynamic
//! per-app handler could only ever touch this same browser state (it never sees
//! the app's `&mut self`), so it would buy indirection without capability.
//!
//! Tools come in two [`ToolKind`]s that share one name space:
//!
//! - **Data** tools — the browser's [`ToolCall`]s and the apps' [`AppTool`]s —
//!   run over that shared state and return JSON.
//! - **UI** tools run against one app's own `&mut self` (move a cursor, open a
//!   pane). An app advertises them with [`App::ui_tools`](crate::App::ui_tools)
//!   and runs them in [`App::call_ui_tool`](crate::App::call_ui_tool). The
//!   registry only knows each one's spec and owning app: calling one through it
//!   yields an [`Intent`](ToolOutcome::Intent) carrying
//!   [`AppAction::CallTool`], which the shell routes to the owner next. That is
//!   how an app gets an intent of its own; the browser still owns every other
//!   kind.
//!
//! A tool is described in two portable halves so one declaration serves every
//! backend:
//!
//! - the **spec** ([`ToolSpec`]) — a backend-agnostic description (name, docs,
//!   argument schema) reused to build an OpenAI `FunctionObject` or an MCP
//!   `tools/list` entry, and
//! - the **execution** ([`ToolCall::call`]) — runs the parsed, typed call
//!   against a [`ToolContext`] and returns a [`ToolOutcome`].
//!
//! A backend advertises [`ToolCall::specs`], parses an incoming call with
//! [`ToolCall::parse`], and dispatches it with [`ToolCall::call`] over a
//! [`ToolContext`] built from an [`AppContext`](crate::AppContext) via
//! [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
//! `note_context()`.

use std::collections::HashMap;

use nostrdb::{Ndb, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::{Accounts, AppAction, AppId, ExplicitPublishApi, NoteCache};

/// The portable tool *type* layer — argument types, args, and the tool spec —
/// lives in [`agentium_core::tool`] so the engine owns it and any platform can
/// share it. notedeck re-exports it here so `notedeck::tool::ToolSpec` (and the
/// crate-root `notedeck::ToolSpec`) keep resolving for the browser-level tool
/// machinery below, which builds specs from these types.
pub use agentium_core::tool::{ToolArg, ToolArgType, ToolKind, ToolSpec};

/// The canonical [`QueryCall`] — the single nostrdb query tool — is likewise
/// owned by the engine and re-exported here, so `notedeck::tool::QueryCall`
/// keeps resolving for the browser [`ToolCall`] below. The browser and the
/// engine's own dave tools share one struct, `to_filter`, `spec`, and
/// note-key execution rather than each maintaining a copy.
pub use agentium_core::tools::QueryCall;

/// The browser state a tool executes against, reborrowed from an
/// [`AppContext`](crate::AppContext) via
/// [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
/// [`NoteContext`](crate::NoteContext). Tools open their own short-lived
/// [`Transaction`]s from [`ndb`](Self::ndb) as needed.
pub struct ToolContext<'a> {
    pub ndb: &'a Ndb,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a Accounts,
    /// Explicit-relay publisher, for tools that *mutate* nostr-backed state
    /// (e.g. an app editing its own board) and must fan the resulting events out
    /// to relays the way the app's own edits do. `None` for a read-only context,
    /// where a mutating tool should report an error rather than silently drop the
    /// change. Relay targets and the signer come from [`accounts`](Self::accounts)
    /// (see [`Accounts::selected_account_private_relays`] and
    /// [`Accounts::selected_filled`]); this supplies the remote publish path
    /// used by [`fan_out_event_frame`](crate::fan_out_event_frame).
    pub publish: Option<ExplicitPublishApi<'a>>,
}

/// The result of running a [`ToolCall`].
///
/// [`Data`](Self::Data) is a pure JSON value with no UI side effects — safe to
/// hand straight back to the backend. [`Intent`](Self::Intent) is a UI side
/// effect the caller marshals as a next-frame [`AppAction`] (e.g. routing a
/// column). [`Error`](Self::Error) reports a failure to relay to the backend.
pub enum ToolOutcome {
    Data(Value),
    Intent(AppAction),
    Error(String),
}

/// A statically-typed, browser-provided tool call, parsed from a backend's
/// request. Each variant owns a concrete argument struct (so a query still calls
/// [`QueryCall::to_filter`] on a real type), and the whole set is what a backend
/// advertises via [`specs`](Self::specs) and dispatches via [`call`](Self::call).
#[derive(Debug, Clone)]
pub enum ToolCall {
    /// A full-text / filtered nostrdb query.
    Query(QueryCall),
}

impl ToolCall {
    /// The specs for every browser-provided tool — what a backend advertises
    /// (OpenAI `tools`, MCP `tools/list`).
    pub fn specs() -> Vec<ToolSpec> {
        vec![QueryCall::spec()]
    }

    /// The registered name of this tool call.
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::Query(_) => QueryCall::NAME,
        }
    }

    /// Parse a backend's tool call by `name` and raw JSON `args` into a typed
    /// [`ToolCall`]. Returns a human-readable error for an unknown tool or
    /// arguments that don't fit the tool's schema.
    pub fn parse(name: &str, args: &Value) -> Result<ToolCall, String> {
        match name {
            QueryCall::NAME => serde_json::from_value::<QueryCall>(args.clone())
                .map(ToolCall::Query)
                .map_err(|e| format!("failed to parse '{name}' arguments: {e}")),
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    /// Execute the call against `cx`, returning what to hand back to the backend
    /// or apply to the UI.
    pub fn call(self, cx: &mut ToolContext) -> ToolOutcome {
        match self {
            ToolCall::Query(query) => {
                let txn = match Transaction::new(cx.ndb) {
                    Ok(txn) => txn,
                    Err(err) => return ToolOutcome::Error(format!("failed to open db: {err}")),
                };
                // Local nostrdb note keys (not portable hex ids) — the engine's
                // single [`QueryCall`] executes to keys.
                ToolOutcome::Data(json!({ "note_keys": query.execute(&txn, cx.ndb).notes }))
            }
        }
    }
}

/// A typed, per-app agent tool contributed by an [`App`](crate::App) over its
/// nostr-backed data.
///
/// Where [`ToolCall`] is the browser's *privileged* built-in tool set — it can
/// emit an [`Intent`](ToolOutcome::Intent) and may grow wider host access — an
/// `AppTool` is an app extension. It sees only a [`ToolContext`] (the same
/// shared db/caches/accounts every tool gets), never the app's private
/// `&mut self`, yet that is enough to do real work: most app data *is* nostr
/// data (a headway board or a notebook node is a nostr event), so the tool's
/// value is its app-specific schema and query logic, not privileged state
/// access. A tool that needs an app's in-memory state is a [`ToolKind::Ui`]
/// tool instead (see [`App::ui_tools`](crate::App::ui_tools)).
///
/// The trait is *typed*: [`Args`](Self::Args) and [`Output`](Self::Output) are
/// plain serde types, so an implementor never juggles [`Value`] by hand. The
/// registry erases those types into a [`RegisteredTool`], which performs the
/// `Value` ⇆ typed conversion at the boundary.
pub trait AppTool: Send + Sync {
    /// The tool's typed arguments, deserialized from the backend's JSON.
    type Args: DeserializeOwned;

    /// The tool's typed result, serialized back to the backend as JSON.
    type Output: Serialize;

    /// The portable [`ToolSpec`] advertised for this tool. Keep it in sync with
    /// [`Args`](Self::Args) by hand (a future revision may derive it).
    fn spec(&self) -> ToolSpec;

    /// Run the tool against `cx` with typed `args`, returning typed output or a
    /// human-readable error to relay to the backend. An `AppTool` produces data
    /// only; a tool with UI side effects is a [`ToolKind::Ui`] tool instead (see
    /// [`App::call_ui_tool`](crate::App::call_ui_tool)).
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String>;
}

/// A type-erased [`AppTool`], ready to store in a [`ToolRegistry`] next to tools
/// with different `Args`/`Output` types.
///
/// `AppTool`'s associated types make it non-object-safe, so it can't be held as
/// `dyn AppTool`. Rather than introduce a second object-safe trait, this struct
/// captures the concrete tool in a boxed closure that does the `Value` ⇆ typed
/// serde conversion, and caches the tool's [`ToolSpec`] up front. Build one with
/// [`RegisteredTool::new`].
/// The erased body of a [`RegisteredTool`]: takes raw JSON args, does the
/// `Value` ⇆ typed serde conversion around the concrete [`AppTool::call`], and
/// returns a [`ToolOutcome`].
type ErasedHandler = Box<dyn Fn(&mut ToolContext, &Value) -> ToolOutcome + Send + Sync>;

pub struct RegisteredTool {
    spec: ToolSpec,
    handler: ErasedHandler,
    /// The app that contributed it, once the shell has said (see
    /// [`owned_by`](Self::owned_by)).
    owner: Option<AppId>,
}

impl RegisteredTool {
    /// Erase a concrete [`AppTool`] into a registrable tool. The returned tool
    /// deserializes incoming JSON into `T::Args`, runs `tool.call`, and
    /// serializes the output into [`ToolOutcome::Data`] — reporting any serde or
    /// execution failure as [`ToolOutcome::Error`].
    pub fn new<T: AppTool + 'static>(tool: T) -> Self {
        let spec = tool.spec();
        let handler = Box::new(move |cx: &mut ToolContext, args: &Value| {
            let args = match serde_json::from_value::<T::Args>(args.clone()) {
                Ok(args) => args,
                Err(err) => return ToolOutcome::Error(format!("invalid arguments: {err}")),
            };
            match tool.call(cx, args) {
                Ok(output) => match serde_json::to_value(&output) {
                    Ok(value) => ToolOutcome::Data(value),
                    Err(err) => ToolOutcome::Error(format!("failed to serialize output: {err}")),
                },
                Err(err) => ToolOutcome::Error(err),
            }
        });
        Self {
            spec,
            handler,
            owner: None,
        }
    }

    /// Tag the tool with the app slot that contributed it, so a command bar
    /// can list it with that app's tools. The shell does this as it collects
    /// the running apps' tools; an app never needs to.
    pub fn owned_by(mut self, owner: AppId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// The app slot that contributed it, if the shell has said.
    pub fn owner(&self) -> Option<AppId> {
        self.owner
    }

    /// The portable spec advertised for this tool.
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// The tool's registered name (shorthand for `self.spec().name()`).
    pub fn name(&self) -> &'static str {
        self.spec.name()
    }

    /// Run the erased tool with raw JSON `args`.
    pub fn call(&self, cx: &mut ToolContext, args: &Value) -> ToolOutcome {
        (self.handler)(cx, args)
    }
}

/// The shell's UI tool that saves the account's keymap overrides, called with
/// `{ "toml": "<the user keymap>" }`. Named here rather than in the chrome so an
/// app with a keymap editor (Notebook's) can raise it as an
/// [`AppAction::CallTool`](crate::AppAction::CallTool) without depending on the
/// chrome.
pub const SAVE_KEYMAP_TOOL: &str = "chrome_save_keymap";

/// A call to a [`ToolKind::Ui`] tool, on its way from whoever raised it (a key
/// binding, a macro step, an agent) to the app that owns the tool. Carried by
/// [`AppAction::CallTool`]; the shell looks the owner up with
/// [`ToolRegistry::ui_owner`] and hands it to
/// [`App::call_ui_tool`](crate::App::call_ui_tool).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    /// The UI tool's registered name, e.g. `headway_cursor_down`.
    pub name: String,
    /// The tool's arguments, `Value::Null` when it takes none.
    pub args: Value,
}

impl ToolInvocation {
    /// A call to the UI tool `name` with `args`.
    pub fn new(name: impl Into<String>, args: Value) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// Who runs a [`ToolKind::Ui`] tool: an app, or the shell around the apps
/// (the chrome's own tools, like toggling its which-key strip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOwner {
    /// The app in this slot.
    App(AppId),
    /// The shell itself.
    Shell,
}

/// A registered [`ToolKind::Ui`] tool: its spec and who runs it. The registry
/// can't call a UI tool itself (it needs its owner's `&mut self`), so it keeps
/// only enough to advertise it and to tell the shell where it goes.
#[derive(Debug, Clone)]
pub struct UiToolEntry {
    spec: ToolSpec,
    owner: ToolOwner,
}

impl UiToolEntry {
    /// Register `spec` as a UI tool run by the app in slot `owner`. The spec is
    /// marked [`ToolKind::Ui`] whether or not the app already did.
    pub fn new(spec: ToolSpec, owner: AppId) -> Self {
        Self {
            spec: spec.ui(),
            owner: ToolOwner::App(owner),
        }
    }

    /// Register `spec` as a UI tool the shell runs itself.
    pub fn shell(spec: ToolSpec) -> Self {
        Self {
            spec: spec.ui(),
            owner: ToolOwner::Shell,
        }
    }

    /// The UI tool's spec, always [`ToolKind::Ui`].
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Who runs this tool.
    pub fn owner(&self) -> ToolOwner {
        self.owner
    }
}

/// A fresh set of app-contributed tools for the host's [`ToolRegistry`], built
/// by the shell from its running apps (see
/// [`App::take_tool_update`](crate::App::take_tool_update)) and applied with
/// [`ToolRegistry::reset`].
#[derive(Default)]
pub struct ToolUpdate {
    /// Data tools, from each running app's [`App::tools`](crate::App::tools).
    pub tools: Vec<RegisteredTool>,
    /// UI tools, from each running app's
    /// [`App::ui_tools`](crate::App::ui_tools), tagged with the app's slot.
    pub ui_tools: Vec<UiToolEntry>,
}

/// One registered tool as [`ToolRegistry::entries`] lists it: its spec and
/// who it belongs to.
#[derive(Clone, Copy)]
pub struct ToolEntry<'a> {
    /// The tool's spec; its [`kind`](ToolSpec::kind) tells data from UI.
    pub spec: &'a ToolSpec,
    /// Who it belongs to: for a UI tool who runs it, for a data tool the app
    /// that contributed it. `None` for a data tool registered without one
    /// (see [`RegisteredTool::owned_by`]).
    pub owner: Option<ToolOwner>,
}

/// Where a registered name points: into the data tools or the UI tools.
#[derive(Clone, Copy)]
enum ToolSlot {
    Data(usize),
    Ui(usize),
}

/// A set of app-contributed tools, data and UI, indexed by one shared name
/// space.
///
/// Rebuilt by the shell from its running apps and exposed read-only on
/// [`AppContext`](crate::AppContext), so a backend can advertise
/// ([`specs`](Self::specs)) and dispatch ([`call`](Self::call)) app tools
/// uniformly. Mirrors [`KindRendererRegistry`](crate::KindRendererRegistry).
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<RegisteredTool>,
    ui_tools: Vec<UiToolEntry>,
    by_name: HashMap<String, ToolSlot>,
}

impl ToolRegistry {
    /// Register an app data tool. A name already taken, by either kind, is
    /// logged and skipped, so the first registration for a name wins
    /// (registration order is app-startup order).
    pub fn register(&mut self, tool: RegisteredTool) {
        let name = tool.name();
        if self.by_name.contains_key(name) {
            tracing::warn!("ignoring duplicate agent tool '{name}'");
            return;
        }
        self.by_name
            .insert(name.to_owned(), ToolSlot::Data(self.tools.len()));
        self.tools.push(tool);
    }

    /// Register an app UI tool, with the same first-wins rule across both kinds
    /// as [`register`](Self::register).
    pub fn register_ui(&mut self, entry: UiToolEntry) {
        let name = entry.spec.name();
        if self.by_name.contains_key(name) {
            tracing::warn!("ignoring duplicate UI tool '{name}'");
            return;
        }
        self.by_name
            .insert(name.to_owned(), ToolSlot::Ui(self.ui_tools.len()));
        self.ui_tools.push(entry);
    }

    /// Replace the whole registry with `update`, data tools first, keeping the
    /// duplicate-skipping rules of [`register`](Self::register). The chrome shell
    /// calls this to re-derive the app-tool set from the currently-running apps
    /// when that set changes, so a backend only ever sees tools whose app is live
    /// (and therefore syncing its data).
    pub fn reset(&mut self, update: ToolUpdate) {
        self.tools.clear();
        self.ui_tools.clear();
        self.by_name.clear();
        for tool in update.tools {
            self.register(tool);
        }
        for entry in update.ui_tools {
            self.register_ui(entry);
        }
    }

    /// The specs for every registered app tool, data tools then UI tools. A
    /// backend that only wants what a model can use filters on
    /// [`ToolSpec::kind`].
    pub fn specs(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools
            .iter()
            .map(RegisteredTool::spec)
            .chain(self.ui_tools.iter().map(UiToolEntry::spec))
    }

    /// Every registered tool with its owner, in [`specs`](Self::specs)' order:
    /// what a command bar lists, before it asks which can run now.
    pub fn entries(&self) -> impl Iterator<Item = ToolEntry<'_>> {
        let data = self.tools.iter().map(|tool| ToolEntry {
            spec: tool.spec(),
            owner: tool.owner.map(ToolOwner::App),
        });
        let ui = self.ui_tools.iter().map(|entry| ToolEntry {
            spec: entry.spec(),
            owner: Some(entry.owner),
        });
        data.chain(ui)
    }

    /// Whether a tool with `name` is registered, of either kind.
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Who runs the UI tool `name`, or `None` when `name` is not a registered
    /// UI tool.
    pub fn ui_owner(&self, name: &str) -> Option<ToolOwner> {
        match self.by_name.get(name)? {
            ToolSlot::Ui(idx) => Some(self.ui_tools[*idx].owner),
            ToolSlot::Data(_) => None,
        }
    }

    /// Dispatch a tool call by `name` with raw JSON `args`. A data tool runs
    /// here and returns its result. A UI tool can't run here, so it returns an
    /// [`Intent`](ToolOutcome::Intent) carrying [`AppAction::CallTool`] for the
    /// shell to route to its owner. An unknown name is an
    /// [`Error`](ToolOutcome::Error).
    pub fn call(&self, cx: &mut ToolContext, name: &str, args: &Value) -> ToolOutcome {
        match self.by_name.get(name) {
            Some(&ToolSlot::Data(idx)) => self.tools[idx].call(cx, args),
            Some(&ToolSlot::Ui(_)) => {
                ToolOutcome::Intent(AppAction::CallTool(ToolInvocation::new(name, args.clone())))
            }
            None => ToolOutcome::Error(format!("unknown tool '{name}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_util::TempNdb, UnknownIds, FALLBACK_PUBKEY};
    use serde::Deserialize;

    /// An app tool whose typed `Args`/`Output` exercise the serde boundary. It
    /// ignores the [`ToolContext`], so registry dispatch is testable without
    /// touching the db.
    struct Echo;

    #[derive(Deserialize)]
    struct EchoArgs {
        message: String,
    }

    #[derive(Serialize)]
    struct EchoOutput {
        echoed: String,
    }

    impl AppTool for Echo {
        type Args = EchoArgs;
        type Output = EchoOutput;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(
                "echo",
                "Echo a message back.",
                vec![ToolArg::new("message", ToolArgType::String, "the message").required(true)],
            )
        }

        fn call(&self, _cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
            Ok(EchoOutput {
                echoed: args.message,
            })
        }
    }

    /// The owned pieces a [`ToolContext`] borrows, over a throwaway db. The
    /// temporary directory is kept so it outlives the borrows.
    struct ToolEnv {
        db: TempNdb,
        note_cache: NoteCache,
        accounts: Accounts,
    }

    /// A live [`ToolContext`]'s pieces over a throwaway db, for exercising
    /// dispatch.
    fn test_tool_env() -> ToolEnv {
        let TempNdb { dir, mut ndb } = TempNdb::new();
        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();
        let accounts = Accounts::new(
            None,
            Vec::new(),
            Vec::new(),
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );
        drop(txn);
        ToolEnv {
            db: TempNdb { ndb, dir },
            note_cache: NoteCache::default(),
            accounts,
        }
    }

    #[test]
    fn registry_dispatches_typed_app_tool() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
        };

        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));

        // A valid, typed call round-trips through serde to Data.
        let out = reg.call(&mut cx, "echo", &json!({ "message": "hi" }));
        let ToolOutcome::Data(value) = out else {
            panic!("expected data");
        };
        assert_eq!(value["echoed"], "hi");
    }

    #[test]
    fn registry_rejects_bad_args_and_unknown_tools() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
        };

        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));

        // Args that don't match the typed struct never reach `call`.
        let bad = reg.call(&mut cx, "echo", &json!({ "wrong": 1 }));
        assert!(matches!(bad, ToolOutcome::Error(e) if e.contains("invalid arguments")));

        // An unknown name is a clean error, not a panic.
        let unknown = reg.call(&mut cx, "nope", &json!({}));
        assert!(matches!(unknown, ToolOutcome::Error(e) if e.contains("unknown tool")));
    }

    #[test]
    fn registry_skips_duplicate_names() {
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));
        reg.register(RegisteredTool::new(Echo));

        // The duplicate is dropped; only one spec is advertised.
        assert_eq!(reg.specs().filter(|s| s.name() == "echo").count(), 1);
        assert!(reg.contains("echo"));
    }

    /// A UI tool spec owned by app slot `owner`.
    fn ui_entry(name: &'static str, owner: usize) -> UiToolEntry {
        UiToolEntry::new(ToolSpec::new(name, "a UI tool", Vec::new()), AppId(owner))
    }

    #[test]
    fn registry_lists_ui_tools_tagged_ui() {
        let mut reg = ToolRegistry::default();
        reg.reset(ToolUpdate {
            tools: vec![RegisteredTool::new(Echo)],
            ui_tools: vec![ui_entry("test_cursor_down", 3)],
        });

        let kinds: Vec<_> = reg.specs().map(|s| (s.name(), s.kind())).collect();
        assert_eq!(
            kinds,
            vec![("echo", ToolKind::Data), ("test_cursor_down", ToolKind::Ui)]
        );
        assert_eq!(
            reg.ui_owner("test_cursor_down"),
            Some(ToolOwner::App(AppId(3)))
        );
        // A data tool has no owner app to route to.
        assert_eq!(reg.ui_owner("echo"), None);
        assert_eq!(reg.ui_owner("nope"), None);
    }

    #[test]
    fn calling_a_ui_tool_yields_a_call_tool_intent() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
        };

        let mut reg = ToolRegistry::default();
        reg.register_ui(ui_entry("test_cursor_down", 0));

        let args = json!({ "count": 2 });
        let out = reg.call(&mut cx, "test_cursor_down", &args);
        let ToolOutcome::Intent(AppAction::CallTool(invocation)) = out else {
            panic!("expected a CallTool intent");
        };
        assert_eq!(invocation, ToolInvocation::new("test_cursor_down", args));
    }

    #[test]
    fn duplicate_names_across_kinds_keep_the_first() {
        // A UI tool registered first keeps its name against a data tool...
        let mut reg = ToolRegistry::default();
        reg.register_ui(ui_entry("echo", 1));
        reg.register(RegisteredTool::new(Echo));
        let kinds: Vec<_> = reg.specs().map(|s| s.kind()).collect();
        assert_eq!(kinds, vec![ToolKind::Ui]);
        assert_eq!(reg.ui_owner("echo"), Some(ToolOwner::App(AppId(1))));

        // ...and a data tool registered first keeps it against a UI tool.
        let mut reg = ToolRegistry::default();
        reg.reset(ToolUpdate {
            tools: vec![RegisteredTool::new(Echo)],
            ui_tools: vec![ui_entry("echo", 1)],
        });
        let kinds: Vec<_> = reg.specs().map(|s| s.kind()).collect();
        assert_eq!(kinds, vec![ToolKind::Data]);
        assert_eq!(reg.ui_owner("echo"), None);
    }

    #[test]
    fn query_spec_json_schema_shape() {
        let schema = QueryCall::spec().json_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
        // Only `limit` is required.
        assert_eq!(schema["required"], Value::Array(vec![Value::from("limit")]));

        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props["search"]["type"], "string");
        assert_eq!(props["limit"]["type"], "number");
        // The default is folded into the description the model sees, with no
        // double-paren regression.
        let limit_desc = props["limit"]["description"].as_str().unwrap();
        assert!(limit_desc.contains("Default: 50"));
        assert!(!limit_desc.contains("))"));
    }

    #[test]
    fn parse_query_call() {
        let args = json!({ "search": "nostr", "limit": 5 });
        let ToolCall::Query(q) = ToolCall::parse("query", &args).unwrap();
        assert_eq!(q.search.as_deref(), Some("nostr"));
        assert_eq!(q.limit, Some(5));
    }

    #[test]
    fn parse_unknown_tool_errors() {
        let err = ToolCall::parse("nope", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn query_defaults() {
        // An empty query is valid and falls back to sensible defaults.
        let ToolCall::Query(q) = ToolCall::parse("query", &json!({})).unwrap();
        assert_eq!(q.limit(), 10);
        assert_eq!(q.kind, None);
    }
}
