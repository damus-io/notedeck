//! The chrome's keymap at work: each frame, before the active app renders,
//! resolve the key press against the app's key scopes and run the bound tool
//! or start the bound macro, and at each frame's end move a running macro on
//! (see [`crate::keymap`] and [`crate::keymap::macros`]).

use super::actions::chrome_handle_app_action;
use super::Chrome;
use crate::keymap::macros::{Advance, MacroRun, Settle, MACRO_POLL_INTERVAL, MACRO_TIMEOUT_SECS};
use crate::keymap::{KeyDispatch, KeySeq, Keymap, Scopes};
use notedeck::{App, AppAction, AppContext, ToolInvocation, ToolOutcome, ToolOwner};

/// What a key press fired, owned, so the engine can be put back before it
/// runs.
enum Fired {
    /// A tool call.
    Tool(ToolInvocation),
    /// The macro with this name.
    Macro(String),
}

impl Chrome {
    /// Resolve this frame's key press against the active app's
    /// [key scopes](App::key_scopes) and [current ref](App::current_ref), and
    /// dispatch what it's bound to: a UI tool goes to the app that owns it, a
    /// data tool runs here and now. A key the keymap takes is swallowed, so
    /// the app rendering after this never sees it; any other key reaches the
    /// app untouched.
    #[profiling::function]
    pub(super) fn dispatch_keys(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        let app = self.apps.get(self.active as usize);
        let scopes = Scopes::new(app.map_or(&[][..], |app| app.key_scopes()))
            .with_current(app.and_then(|app| app.current_ref()));

        // The engine is lent out while it asks `self` which tools are
        // available; an empty `KeyEngine` doesn't allocate.
        let mut keys = std::mem::take(&mut self.keys);
        let fired = match keys.resolve(ui.ctx(), scopes, |name| {
            self.key_tool_available(&ctx.registries.tools, name)
        }) {
            None => None,
            Some(KeyDispatch::Tool(invocation)) => Some(Fired::Tool(invocation)),
            Some(KeyDispatch::Macro(name)) => Some(Fired::Macro(name.to_owned())),
        };
        self.keys = keys;

        match fired {
            None => {}
            Some(Fired::Tool(invocation)) => {
                let action = if is_data_tool(ctx, &invocation.name) {
                    call_data_tool(ctx, &invocation)
                } else {
                    Some(AppAction::CallTool(invocation))
                };
                if let Some(action) = action {
                    chrome_handle_app_action(self, ctx, action, ui);
                }
            }
            Some(Fired::Macro(name)) => self.start_macro(ctx, name, ui),
        }
    }

    /// Start the macro `name` a key fired: run its first step now, and leave
    /// the rest to [`advance_macro`](Self::advance_macro). A macro already
    /// running is dropped for it. One with a step no app advertises, or whose
    /// first step fails, is aborted at once.
    fn start_macro(&mut self, ctx: &mut AppContext, name: String, ui: &mut egui::Ui) {
        if let Some(old) = self.macro_run.take() {
            tracing::info!(
                "keymap: macro '{name}' replaces '{}', dropped at step {}",
                old.name(),
                old.next_step() + 1
            );
        }
        let Some(m) = self.keys.keymap().macro_named(&name) else {
            tracing::warn!("keymap: macro '{name}' is not in the keymap");
            return;
        };
        if let Some(unknown) = m
            .steps
            .iter()
            .map(|step| step.invocation.name.as_str())
            .find(|tool| !self.tool_known(ctx, tool))
        {
            tracing::warn!("keymap: macro '{name}' aborted: no app has a tool '{unknown}'");
            return;
        }
        let steps = m.steps.len();
        let first = m.steps[0].invocation.clone();

        let now = ui.input(|i| i.time);
        let mut run = MacroRun::new(name, now);
        if let Err(err) = self.run_macro_step(ctx, &first, ui) {
            tracing::warn!(
                "keymap: macro '{}' aborted at step 1 ('{}'): {err}",
                run.name(),
                first.name
            );
            return;
        }
        if run.dispatched(now, steps) {
            self.macro_run = Some(run);
            ui.ctx().request_repaint_after(MACRO_POLL_INTERVAL);
        }
    }

    /// Move the running macro on, at the end of a frame once its navigation
    /// has been applied: run its next step if the last one has settled (see
    /// [`MacroRun::poll`]), abort it if that took too long, or else wait for
    /// another frame. Does nothing, and allocates nothing, with no macro
    /// running.
    #[profiling::function]
    pub(super) fn advance_macro(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        let Some(mut run) = self.macro_run.take() else {
            return;
        };
        let Some(m) = self.keys.keymap().macro_named(run.name()) else {
            tracing::warn!(
                "keymap: macro '{}' left the keymap while running; aborted",
                run.name()
            );
            return;
        };
        let steps = m.steps.len();
        let Some(step) = m.steps.get(run.next_step()) else {
            return;
        };
        let now = ui.input(|i| i.time);
        let settle = self.macro_settle(ctx, &step.invocation.name);
        match run.poll(now, settle) {
            Advance::Wait => {
                self.macro_run = Some(run);
                ui.ctx().request_repaint_after(MACRO_POLL_INTERVAL);
            }
            Advance::Abort(at, why) => {
                tracing::warn!(
                    "keymap: macro '{}' aborted at step {} ('{}'): {why} within {MACRO_TIMEOUT_SECS}s",
                    run.name(),
                    at + 1,
                    step.invocation.name
                );
            }
            Advance::Run(at) => {
                let invocation = step.invocation.clone();
                if let Err(err) = self.run_macro_step(ctx, &invocation, ui) {
                    tracing::warn!(
                        "keymap: macro '{}' aborted at step {} ('{}'): {err}",
                        run.name(),
                        at + 1,
                        invocation.name
                    );
                    return;
                }
                if run.dispatched(now, steps) {
                    self.macro_run = Some(run);
                    ui.ctx().request_repaint_after(MACRO_POLL_INTERVAL);
                }
            }
        }
    }

    /// Run one macro step: a data tool here, a UI tool in its app, and an
    /// intent either raises routed like any other action. A step that fails,
    /// or answers by calling another tool (a chain is the macro's job, so
    /// that is one hop too many), is an error.
    fn run_macro_step(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
        ui: &mut egui::Ui,
    ) -> Result<(), String> {
        let outcome = if is_data_tool(ctx, &invocation.name) {
            ctx.call_tool(&invocation.name, &invocation.args)
        } else {
            self.call_ui_tool(ctx, invocation)
        };
        match outcome {
            ToolOutcome::Data(_) => Ok(()),
            ToolOutcome::Error(err) => Err(err),
            ToolOutcome::Intent(AppAction::CallTool(next)) => Err(format!(
                "it asked to call '{}'; a step is one call",
                next.name
            )),
            ToolOutcome::Intent(action) => {
                chrome_handle_app_action(self, ctx, action, ui);
                Ok(())
            }
        }
    }

    /// Whether the world has settled enough for a macro step calling `tool`
    /// to run (see [`Settle`]). Doesn't allocate.
    fn macro_settle(&self, ctx: &AppContext, tool: &str) -> Settle {
        let tools = &ctx.registries.tools;
        let owner_active = match tools.ui_owner(tool) {
            Some(ToolOwner::App(owner)) => self.active_slot() == Some(owner.slot()),
            Some(ToolOwner::Shell) => true,
            // A data tool runs here, whichever app is in front; one no
            // running app has registered yet waits for its app to start.
            None => is_registered_data_tool(tools, tool),
        };
        Settle {
            open_pending: self.pending_open.is_some(),
            nav_queued: !ctx.navigator.is_empty(),
            owner_active,
            available: self.key_tool_available(tools, tool),
        }
    }

    /// Whether some app has a tool called `name`, running or not, or it is
    /// the chrome's own. An app that isn't running has no tools in the
    /// registry yet, so this asks the apps themselves, which allocates: for a
    /// macro starting and a keymap loading, not for every frame.
    fn tool_known(&self, ctx: &AppContext, name: &str) -> bool {
        self.chrome_tool_available(name).is_some()
            || ctx.registries.tools.contains(name)
            || self.apps.iter().any(|app| {
                app.ui_tools().iter().any(|spec| spec.name() == name)
                    || app.tools().iter().any(|tool| tool.name() == name)
            })
    }

    /// The tools the keymap's macros call that no app has, one line each, so
    /// a keymap that can never run a macro says so when it loads. A binding's
    /// own tool isn't checked: one whose app isn't built in just never fires.
    pub fn keymap_problems(&self, ctx: &AppContext) -> Vec<String> {
        self.keys
            .keymap()
            .macro_problems(|tool| self.tool_known(ctx, tool))
    }

    /// Log [`keymap_problems`](Self::keymap_problems), for a keymap just
    /// loaded or a roster just built.
    pub(super) fn log_keymap_problems(&self, ctx: &AppContext) {
        for problem in self.keymap_problems(ctx) {
            tracing::warn!("keymap: {problem}");
        }
    }

    /// Whether the tool `name` a key is bound to would do something now: a
    /// UI tool its owner says is [available](Self::ui_tool_available), or any
    /// registered data tool (which acts on what its args name, so only the
    /// keymap's [`CURRENT`](crate::keymap::CURRENT) gate applies). Doesn't
    /// allocate.
    pub fn key_tool_available(&self, tools: &notedeck::ToolRegistry, name: &str) -> bool {
        self.ui_tool_available(tools, name) || is_registered_data_tool(tools, name)
    }

    /// The keymap the chrome resolves keys against.
    pub fn keymap(&self) -> &Keymap {
        self.keys.keymap()
    }

    /// The keys of a chord in progress (the `g` of `g g`), if any.
    pub fn pending_keys(&self) -> Option<KeySeq> {
        self.keys.pending()
    }

    /// Replace the keymap, e.g. with one parsed from a user's overrides. Any
    /// chord in progress, and any macro running, is dropped.
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keys.set_keymap(keymap);
        self.macro_run = None;
    }

    /// The name of the macro running, if one is waiting on its next step.
    pub fn running_macro(&self) -> Option<&str> {
        self.macro_run.as_ref().map(MacroRun::name)
    }
}

/// Whether `name` is an app's data tool in `tools`, not a UI tool (nor the
/// chrome's own, which never registers).
fn is_registered_data_tool(tools: &notedeck::ToolRegistry, name: &str) -> bool {
    tools.contains(name) && tools.ui_owner(name).is_none()
}

/// Whether the key's tool `name` is an app's data tool, to run here rather
/// than route to an app.
fn is_data_tool(ctx: &AppContext, name: &str) -> bool {
    is_registered_data_tool(&ctx.registries.tools, name)
}

/// Run a data tool a key is bound to, returning the intent it raised, if
/// any, for the chrome to route. Its data is for an agent; a key only wants
/// its effect (a board edit, which the app picks up as it folds in), so only
/// a failure is worth a line in the log.
fn call_data_tool(ctx: &mut AppContext, invocation: &ToolInvocation) -> Option<AppAction> {
    match ctx.call_tool(&invocation.name, &invocation.args) {
        ToolOutcome::Data(_) => None,
        ToolOutcome::Intent(action) => Some(action),
        ToolOutcome::Error(err) => {
            tracing::warn!("keymap: '{}' failed: {err}", invocation.name);
            None
        }
    }
}
