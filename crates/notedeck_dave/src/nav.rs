//! Dave's entries on its chrome-owned nav stack (see [`notedeck::Navigator`]).
//!
//! Switching sessions from the session list is not an entry: the list is the
//! navigation for those. Opening a subagent's virtual session from its block
//! in the parent's chat is one, so the chrome's back (the header chevron,
//! Alt-←) returns to the parent's chat with the cursor on that block.
//!
//! The open pushes a [`DaveRoute::Subagent`]. When a back pops it, the chrome
//! hands it to [`App::cleanup_nav`](notedeck::App::cleanup_nav), and
//! [`Dave::leave_route`] goes to the parent. When a forward replays it, it is
//! the top token again, and [`Dave::seed_from_route`] opens the subagent once.
//! Dave draws itself the same either way; the token only says which session
//! to make active.

use std::any::Any;
use std::rc::Rc;

use crate::session::SessionId;
use crate::ui::subagent_block_id;
use crate::update::{self, InputFocus};
use crate::Dave;

/// A view of Dave's that the chrome's back and forward step between.
#[derive(Clone, Debug, PartialEq)]
pub enum DaveRoute {
    /// The virtual session of `parent`'s subagent `task_id`, opened from its
    /// block in `parent`'s chat.
    Subagent { parent: SessionId, task_id: String },
}

/// The token Dave last seeded its view from, so [`Dave::seed_from_route`]
/// acts once per change of the stack's top rather than every frame, and a
/// session switch made while on an entry sticks.
#[derive(Default)]
pub(crate) struct SeenRoute {
    token: Option<Rc<dyn Any>>,
}

impl SeenRoute {
    /// Record `token` as the top; true when it differs from the last one.
    fn changed_to(&mut self, token: &Rc<dyn Any>) -> bool {
        if self
            .token
            .as_ref()
            .is_some_and(|seen| Rc::ptr_eq(seen, token))
        {
            return false;
        }
        self.token = Some(token.clone());
        true
    }
}

impl Dave {
    /// Make the view match the stack's top `token` when it changed since the
    /// last frame: a subagent entry opens its session. The root and tokens
    /// that aren't Dave's leave the view alone.
    pub(crate) fn seed_from_route(&mut self, token: &Rc<dyn Any>) {
        if !self.seen_route.changed_to(token) {
            return;
        }
        let Some(DaveRoute::Subagent { parent, task_id }) = token.downcast_ref::<DaveRoute>()
        else {
            return;
        };
        // A row that appeared since may not have its session yet.
        self.session_manager.sync_subagent_sessions();
        let Some(id) = self.session_manager.subagent_session(*parent, task_id) else {
            return;
        };
        if self.session_manager.active_id() != Some(id) {
            self.switch_for_nav(id);
        }
    }

    /// An entry left the stack (a back popped it): a subagent entry returns
    /// to its parent's chat, the cursor on the subagent's block. A parent
    /// that is gone since makes it a no-op.
    pub(crate) fn leave_route(&mut self, token: &Rc<dyn Any>) {
        let Some(DaveRoute::Subagent { parent, task_id }) = token.downcast_ref::<DaveRoute>()
        else {
            return;
        };
        if self.session_manager.get(*parent).is_none() {
            return;
        }
        self.switch_for_nav(*parent);
        let Some(session) = self.session_manager.get_mut(*parent) else {
            return;
        };
        session.inspector_view.show_chat();
        session.block_nav.seek(subagent_block_id(task_id));
    }

    /// The history dropdown's title for `token`: the subagent's session
    /// title, while it has a session.
    pub(crate) fn route_title(&self, token: &Rc<dyn Any>) -> Option<String> {
        let DaveRoute::Subagent { parent, task_id } = token.downcast_ref::<DaveRoute>()?;
        let id = self.session_manager.subagent_session(*parent, task_id)?;
        let session = self.session_manager.get(id)?;
        Some(session.details.display_title().to_owned())
    }

    /// Switch to `id` the way the session list does, keyboard left where it
    /// is.
    fn switch_for_nav(&mut self, id: SessionId) {
        update::switch_session(
            &mut self.session_manager,
            &mut self.scene,
            self.show_scene,
            id,
            InputFocus::Leave,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::messages::{Message, SubagentInfo, SubagentStatus};
    use crate::test_support::TestDave;
    use crate::ui::{self, DaveAction, UiActionResult};

    /// A parent session with one running subagent, `toolu_a`, whose virtual
    /// session exists.
    fn with_subagent(test: &mut TestDave) -> SessionId {
        let [parent] = test.add_sessions(["/tmp/parent"], BackendType::Claude);
        let sm = &mut test.dave.session_manager;
        sm.get_mut(parent)
            .unwrap()
            .insert_turn_content(Message::Subagent(SubagentInfo {
                task_id: "toolu_a".into(),
                description: "find the parser".into(),
                subagent_type: "Explore".into(),
                status: SubagentStatus::Running,
                output: String::new(),
                max_output_size: 4000,
                tool_results: Vec::new(),
                background: false,
                agent_id: None,
                transcript: Default::default(),
            }));
        parent
    }

    /// The parent's `open` on the subagent's block, as the chat raises it:
    /// the subagent's session becomes active, and the route it hands back
    /// is the entry the chrome pushes.
    fn open_subagent(test: &mut TestDave) -> Rc<dyn Any> {
        let dave = &mut test.dave;
        let result = ui::handle_ui_action(
            DaveAction::OpenSubagent("toolu_a".into()),
            &mut dave.session_manager,
            &crate::backend::RemoteOnlyBackend,
            &mut crate::DaveOverlay::None,
            &mut false,
            &egui::Context::default(),
        );
        let UiActionResult::OpenedSubagent(route) = result else {
            panic!("opening a subagent is a nav entry");
        };
        Rc::new(route)
    }

    /// Open a subagent, then back: the parent's chat is active again, its
    /// cursor seeking the subagent's block. Forward opens the subagent again.
    #[test]
    fn back_from_a_subagent_returns_to_the_parent() {
        let mut test = TestDave::new();
        let parent = with_subagent(&mut test);
        let root: Rc<dyn Any> = Rc::new(());
        test.dave.seed_from_route(&root);

        let entry = open_subagent(&mut test);
        let dave = &mut test.dave;
        let sub = dave.session_manager.subagent_session(parent, "toolu_a");
        assert!(sub.is_some());
        assert_eq!(dave.session_manager.active_id(), sub);
        dave.seed_from_route(&entry);
        assert_eq!(dave.session_manager.active_id(), sub, "pushed, still there");
        assert_eq!(
            dave.route_title(&entry).as_deref(),
            Some("Explore: find the parser")
        );

        // The back's slide lands: the chrome pops the entry and the root is
        // the top again.
        dave.leave_route(&entry);
        dave.seed_from_route(&root);
        assert_eq!(dave.session_manager.active_id(), Some(parent));
        let session = dave.session_manager.get(parent).unwrap();
        assert_eq!(
            session.block_nav.seeking(),
            Some(subagent_block_id("toolu_a")),
            "the cursor goes to the subagent's block"
        );

        // Forward replays the same entry.
        dave.seed_from_route(&entry);
        assert_eq!(dave.session_manager.active_id(), sub);
    }

    /// While on the subagent's entry, a switch from the session list sticks:
    /// the entry seeds the view only when it becomes the top.
    #[test]
    fn a_list_switch_on_a_subagent_entry_sticks() {
        let mut test = TestDave::new();
        let parent = with_subagent(&mut test);
        let entry = open_subagent(&mut test);
        let dave = &mut test.dave;
        dave.seed_from_route(&entry);

        dave.session_manager.switch_to(parent);
        dave.seed_from_route(&entry);
        assert_eq!(dave.session_manager.active_id(), Some(parent));
    }

    /// A back to a parent deleted since does nothing.
    #[test]
    fn back_to_a_deleted_parent_is_a_no_op() {
        let mut test = TestDave::new();
        let parent = with_subagent(&mut test);
        let [other] = test.add_sessions(["/tmp/other"], BackendType::Claude);
        test.dave.session_manager.switch_to(parent);
        let entry = open_subagent(&mut test);
        let dave = &mut test.dave;

        dave.session_manager.switch_to(other);
        dave.delete_session(parent);
        dave.leave_route(&entry);
        assert_eq!(dave.session_manager.active_id(), Some(other));
        assert_eq!(dave.route_title(&entry), None);
    }
}
