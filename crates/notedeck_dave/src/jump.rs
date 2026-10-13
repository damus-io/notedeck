//! Dave's half of the chrome's jump list (vim's Ctrl-O / Ctrl-I): what a
//! location in Dave is, and putting the view back at one.
//!
//! The chrome records a location before each keymap binding marked `jump`
//! (`g g` and `G` in the chat and the session list, `] q` / `[ q`); Dave
//! records its own before the hard-coded session switches (Ctrl-1..9,
//! Ctrl-J/K, Ctrl-N/P), which don't go through the keymap.

use std::rc::Rc;

use notedeck::{AppAction, JumpLocation, JumpTarget};

use crate::inspector::SessionTab;
use crate::session::SessionId;
use crate::ui::keybindings::{KeyAction, Pane};
use crate::ui::BlockSpot;
use crate::update::{self, InputFocus};
use crate::Dave;

/// Where Dave's view is: the session, and where in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DaveJump {
    /// The active session.
    session: SessionId,
    /// The pane normal mode's motions move through.
    pane: Pane,
    /// The chat's block cursor, if it has one.
    block: Option<BlockSpot>,
    /// The session's tab: the chat or its details.
    tab: SessionTab,
    /// Whether the scene's map stands in for the session list.
    scene: bool,
}

impl Dave {
    /// Where the view is now, or `None` with no active session.
    pub(crate) fn jump_here(&self) -> Option<DaveJump> {
        let session = self.session_manager.get_active()?;
        Some(DaveJump {
            session: session.id,
            pane: self.normal_mode.pane(),
            block: session.block_nav.spot(),
            tab: session.inspector_view.tab,
            scene: self.show_scene,
        })
    }

    /// Put the view back at `to`. A session that is gone since makes it a
    /// no-op. Switching sessions leaves the keyboard where it is, as normal
    /// mode's session list does.
    pub(crate) fn jump_to(&mut self, to: &DaveJump) {
        if self.session_manager.get(to.session).is_none() {
            return;
        }
        self.show_scene = to.scene;
        if self.session_manager.active_id() != Some(to.session) {
            update::switch_session(
                &mut self.session_manager,
                &mut self.scene,
                to.scene,
                to.session,
                InputFocus::Leave,
            );
        }
        self.normal_mode.set_pane(to.pane);
        let Some(session) = self.session_manager.get_mut(to.session) else {
            return;
        };
        session.inspector_view.tab = to.tab;
        session.block_nav.restore(to.block);
    }

    /// [`jump_here`](Self::jump_here) as the chrome keeps it.
    pub(crate) fn jump_location_token(&self) -> Option<JumpLocation> {
        self.jump_here()
            .map(|here| Rc::new(here) as Rc<dyn JumpTarget>)
    }

    /// [`jump_to`](Self::jump_to) a location the chrome handed back; one
    /// that isn't Dave's is a no-op.
    pub(crate) fn restore_jump_token(&mut self, location: &dyn JumpTarget) {
        if let Some(to) = location.as_any().downcast_ref::<DaveJump>() {
            self.jump_to(to);
        }
    }

    /// Run a hard-coded key's `action` (see
    /// [`dispatch_key_action`](Self::dispatch_key_action)), recording a jump
    /// first when it is a session switch that moved the view, since those
    /// keys never reach the chrome's keymap.
    pub(crate) fn dispatch_with_jump(
        &mut self,
        action: KeyAction,
        egui_ctx: &egui::Context,
    ) -> Option<AppAction> {
        if !is_jump(&action) {
            return self.dispatch_key_action(action, egui_ctx);
        }
        let from = self.jump_here();
        let raised = self.dispatch_key_action(action, egui_ctx);
        let Some(from) = from else {
            return raised;
        };
        if raised.is_some() || self.jump_here() == Some(from) {
            return raised;
        }
        Some(AppAction::RecordJump(Rc::new(from)))
    }
}

/// Whether a hard-coded key's `action` is a far jump: one that switches
/// sessions.
fn is_jump(action: &KeyAction) -> bool {
    matches!(
        action,
        KeyAction::SwitchToAgent(_)
            | KeyAction::NextAgent
            | KeyAction::PreviousAgent
            | KeyAction::FocusQueueNext
            | KeyAction::FocusQueuePrev
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::test_support::TestDave;

    /// The chat's block cursor on block 3, `G` to the last, and the jump
    /// back puts it on block 3 again.
    #[test]
    fn a_jump_back_restores_the_block_cursor() {
        let mut test = TestDave::new();
        let [id] = test.add_sessions(["/tmp/a"], BackendType::Claude);
        let dave = &mut test.dave;
        let nav = &mut dave.session_manager.get_mut(id).unwrap().block_nav;
        nav.seed_blocks(6);
        nav.seed_cursor(3);

        let before = dave.jump_location_token().expect("a session is active");
        let nav = &mut dave.session_manager.get_mut(id).unwrap().block_nav;
        nav.last();
        assert_eq!(nav.cursor(), Some(5));
        let after = dave.jump_location_token().unwrap();
        assert!(!before.same(&*after), "the cursor moved");

        dave.restore_jump_token(&*before);
        let nav = &dave.session_manager.get(id).unwrap().block_nav;
        assert_eq!(nav.cursor(), Some(3));
        dave.restore_jump_token(&*after);
        let nav = &dave.session_manager.get(id).unwrap().block_nav;
        assert_eq!(nav.cursor(), Some(5), "and forward to the last again");
    }

    /// A jump back to another session switches to it, with its tab; one to
    /// a session deleted since does nothing.
    #[test]
    fn a_jump_back_switches_sessions_and_skips_deleted_ones() {
        let mut test = TestDave::new();
        let [a, b] = test.add_sessions(["/tmp/a", "/tmp/b"], BackendType::Claude);
        let dave = &mut test.dave;
        dave.session_manager.get_mut(a).unwrap().inspector_view.tab = SessionTab::Details;
        let at_a = dave.jump_location_token().unwrap();

        dave.session_manager.switch_to(b);
        dave.restore_jump_token(&*at_a);
        assert_eq!(dave.session_manager.active_id(), Some(a));
        assert_eq!(
            dave.session_manager.get(a).unwrap().inspector_view.tab,
            SessionTab::Details
        );

        dave.session_manager.switch_to(b);
        dave.delete_session(a);
        dave.restore_jump_token(&*at_a);
        assert_eq!(dave.session_manager.active_id(), Some(b), "a is gone");
    }

    /// Ctrl-J (next agent) is a hard-coded key: Dave raises the jump
    /// itself, from where it was.
    #[test]
    fn a_hard_coded_session_switch_records_a_jump() {
        let mut test = TestDave::new();
        let [a, _b] = test.add_sessions(["/tmp/a", "/tmp/b"], BackendType::Claude);
        let ctx = egui::Context::default();
        let raised = test.dave.dispatch_with_jump(KeyAction::NextAgent, &ctx);
        let Some(AppAction::RecordJump(from)) = raised else {
            panic!("a session switch records a jump");
        };
        let from = from.as_any().downcast_ref::<DaveJump>().unwrap();
        assert_eq!(from.session, a);
    }
}
