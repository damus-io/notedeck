//! UI tool calls queued with a reply ([`AppActionQueue::call_tool`]), from a
//! caller off the frame that needs the tool's answer: Dave's native agent
//! loop, when a model calls a UI tool.
//!
//! The chrome takes them in its `update`, after the apps' (which is where
//! Dave queues them), so a call is answered on the tick it was made, headless
//! too. A tool's data or error is answered there and then. An intent the tool
//! raises needs the window: it is held for the next render, which routes it as
//! a socket client's intent is routed, and answered
//! [`Applied`](UiToolAnswer::Applied). With no window it is answered with an
//! error.
//!
//! ```text
//! update:  Dave ─ call_tool ─▶ app_actions ─▶ answer_queued_tool_calls ─ run_ui_tool
//!                                                 Data / Error ─────────────▶ reply
//!                                                 intent, window ─▶ held
//!                                                 intent, headless ─ Error ─▶ reply
//! render:  apply_queued_tool_intents ─ route held ─ Applied ─────────────────▶ reply
//! ```
//!
//! [`AppActionQueue::call_tool`]: notedeck::AppActionQueue::call_tool

use notedeck::{AppAction, AppContext, ReplyingCall, ToolOutcome, UiToolAnswer, UiToolReply};

use super::actions::chrome_handle_app_action;
use super::Chrome;

/// The intents queued UI tool calls raised, held for the next render. Empty
/// but for the tick a call arrives on.
#[derive(Default)]
pub(super) struct QueuedCalls {
    applying: Vec<HeldIntent>,
}

/// An intent a queued UI tool call raised, and where to say it was applied.
struct HeldIntent {
    action: AppAction,
    reply: UiToolReply,
}

impl Chrome {
    /// Answer every UI tool call queued with a reply this tick (see the
    /// module docs). Allocates nothing when none are queued.
    pub(super) fn answer_queued_tool_calls(&mut self, ctx: &mut AppContext) {
        let calls = ctx.app_actions.take_tool_calls();
        if calls.is_empty() {
            return;
        }
        for ReplyingCall { invocation, reply } in calls {
            let action = match self.run_ui_tool(ctx, &invocation) {
                ToolOutcome::Data(value) => {
                    reply.send(UiToolAnswer::Data(value));
                    continue;
                }
                ToolOutcome::Error(err) => {
                    reply.send(UiToolAnswer::Error(err));
                    continue;
                }
                ToolOutcome::Intent(action) => action,
            };
            let Some(egui) = ctx.egui else {
                reply.send(UiToolAnswer::Error(format!(
                    "'{}' acts on the window, and this notedeck is headless",
                    invocation.name
                )));
                continue;
            };
            egui.request_repaint();
            self.queued_calls
                .applying
                .push(HeldIntent { action, reply });
        }
    }

    /// Carry out the intents queued UI tool calls raised, and answer each.
    /// Allocates nothing when none are waiting.
    pub(super) fn apply_queued_tool_intents(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        if self.queued_calls.applying.is_empty() {
            return;
        }
        for HeldIntent { action, reply } in std::mem::take(&mut self.queued_calls.applying) {
            chrome_handle_app_action(self, ctx, action, ui);
            reply.send(UiToolAnswer::Applied);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ipc::tests::{fixture, CALL_AGAIN, PEEK, TOGGLE};
    use notedeck::{ToolInvocation, UiToolAnswer, UiToolPending};
    use serde_json::{json, Value};

    /// Queue a call of `name` on the fixture's app context, then let the
    /// chrome answer it as its `update` would, with a window when `egui` is
    /// given.
    fn queue_and_answer(
        f: &mut super::super::ipc::tests::Fixture,
        name: &str,
        args: Value,
        egui: Option<&egui::Context>,
    ) -> UiToolPending {
        let mut ctx = f.notedeck.app_context();
        ctx.egui = egui;
        let pending = ctx.app_actions.call_tool(ToolInvocation::new(name, args));
        f.chrome.answer_queued_tool_calls(&mut ctx);
        pending
    }

    /// Headless, a UI tool's data reaches the caller on the tick it called.
    #[tokio::test]
    async fn a_ui_tools_data_comes_back_headless() {
        let mut f = fixture();
        let pending = queue_and_answer(&mut f, PEEK, Value::Null, None);
        assert_eq!(
            pending.try_answer(),
            Some(UiToolAnswer::Data(json!({ "peeked": true })))
        );
        assert_eq!(*f.calls.borrow(), vec![PEEK]);
    }

    /// Headless, an intent has no window to carry it out: an error, not a
    /// silent drop.
    #[tokio::test]
    async fn an_intent_is_an_error_headless() {
        let mut f = fixture();
        let pending = queue_and_answer(&mut f, TOGGLE, Value::Null, None);
        let Some(UiToolAnswer::Error(err)) = pending.try_answer() else {
            panic!("a headless intent is an error");
        };
        assert!(err.contains("headless"), "{err}");
    }

    /// A tool that asks to call another is refused, as on the socket.
    #[tokio::test]
    async fn a_queued_call_is_one_hop() {
        let mut f = fixture();
        let pending = queue_and_answer(&mut f, CALL_AGAIN, Value::Null, None);
        let Some(UiToolAnswer::Error(err)) = pending.try_answer() else {
            panic!("a second hop is an error");
        };
        assert!(err.contains("one hop"), "{err}");
        assert_eq!(*f.calls.borrow(), vec![CALL_AGAIN], "PEEK never ran");
    }

    /// With a window, an intent is held until the render routes it, and
    /// only then answered `Applied`.
    #[tokio::test]
    async fn an_intent_is_applied_by_the_render() {
        let mut f = fixture();
        let egui_ctx = egui::Context::default();
        let pending = queue_and_answer(&mut f, TOGGLE, Value::Null, Some(&egui_ctx));
        assert_eq!(pending.try_answer(), None, "held for the render");

        let mut ctx = f.notedeck.app_context();
        egui_ctx
            .run_ui(egui::RawInput::default(), |ui| {
                f.chrome.apply_queued_tool_intents(&mut ctx, ui);
            })
            .drop_without_applying_deltas();
        assert!(f.chrome.nav.navigating, "the drawer is opening");
        assert_eq!(pending.try_answer(), Some(UiToolAnswer::Applied));
    }

    /// A call the host drops untaken (no shell ran its `update`) tells the
    /// waiter so rather than leaving it to its bound.
    #[test]
    fn an_untaken_call_answers_no_shell() {
        let mut queue = notedeck::AppActionQueue::default();
        let pending = queue.call_tool(ToolInvocation::new(PEEK, Value::Null));
        drop(queue);
        let answer = pending.wait(std::time::Duration::from_secs(5), || false);
        assert_eq!(
            answer,
            UiToolAnswer::Error("no shell ran the call".to_owned())
        );
    }

    /// The real Columns `nostr_open_thread`, called with a malformed id,
    /// reaches the caller as the tool's error instead of a warn in the log.
    #[tokio::test]
    async fn a_malformed_open_thread_reaches_the_caller_as_an_error() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let mut chrome = super::Chrome::new_headless(&args, &mut notedeck).expect("chrome");
        let update = notedeck::App::take_tool_update(&mut chrome).expect("tools");
        notedeck.apply_tool_update(update);

        let egui_ctx = egui::Context::default();
        let mut ctx = notedeck.app_context();
        ctx.egui = Some(&egui_ctx);
        let pending = ctx.app_actions.call_tool(ToolInvocation::new(
            "nostr_open_thread",
            json!({ "note": "not-a-note-id" }),
        ));
        chrome.answer_queued_tool_calls(&mut ctx);
        let Some(UiToolAnswer::Error(err)) = pending.try_answer() else {
            panic!("a malformed id is the tool's error");
        };
        assert!(err.contains("is not a note id"), "{err}");
    }
}
