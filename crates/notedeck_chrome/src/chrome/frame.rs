//! The chrome's per-frame layout: the side drawer over the nav frame that
//! renders the active app's stack, with the desktop tab strip above it and the
//! mobile toolbar and soft-keyboard inset below.

use super::actions::{chrome_handle_app_action, retry_pending_open};
use super::header::chrome_app_tabs;
use super::keyboard::{keyboard_visibility, virtual_keyboard_ui};
use super::sidebar::{milestone_name, topdown_sidebar, SidebarOptions};
use super::toolbar::{chrome_toolbar, toolbar_visibility_height, ChromeToolbarAction};
use super::{Chrome, ChromePanelAction, ChromeRoute};
use crate::ChromeOptions;
use egui::{CornerRadius, Layout, Margin};
use egui_extras::{Size, StripBuilder};
use egui_nav::{NavAction, NavDrawer, RouteResponse};
use notedeck::ui::is_compiled_as_mobile;
use notedeck::{App, AppContext, NavPass};

impl Chrome {
    /// The chrome side panel
    #[profiling::function]
    fn panel(
        &mut self,
        app_ctx: &mut AppContext,
        ui: &mut egui::Ui,
        amt_keyboard_open: f32,
    ) -> Option<ChromePanelAction> {
        let drawer = NavDrawer::new(&ChromeRoute::App, &ChromeRoute::Chrome)
            .navigating(self.nav.navigating)
            .returning(self.nav.returning)
            .drawer_focused(self.nav.drawer_focused)
            .drag(is_compiled_as_mobile())
            .opened_offset(240.0);

        let resp = drawer.show_mut(ui, |ui, route| match route {
            ChromeRoute::Chrome => {
                ui.painter().rect_filled(
                    ui.available_rect_before_wrap(),
                    CornerRadius::ZERO,
                    if ui.visuals().dark_mode {
                        egui::Color32::BLACK
                    } else {
                        egui::Color32::WHITE
                    },
                );
                egui::Frame::new()
                    .inner_margin(Margin::same(notedeck::tokens::SPACING_LG as i8))
                    .show(ui, |ui| {
                        let options = if amt_keyboard_open > 0.0 {
                            SidebarOptions::Compact
                        } else {
                            SidebarOptions::default()
                        };

                        let response = ui
                            .with_layout(Layout::top_down(egui::Align::Min), |ui| {
                                topdown_sidebar(self, app_ctx, ui, options)
                            })
                            .inner;

                        ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                            ui.add(milestone_name(app_ctx.i18n));
                        });

                        RouteResponse {
                            response,
                            can_take_drag_from: Vec::new(),
                        }
                    })
                    .inner
            }
            ChromeRoute::App => {
                let animate = app_ctx.settings.get_settings_mut().animate_nav_transitions;

                // The app's area, for the which-key strip to float over once
                // the app has drawn in all of it.
                let app_rect = ui.available_rect_before_wrap();

                // The app's action is bubbled out of the pass and routed
                // *after* it returns, since the body can't reborrow the whole
                // `self` that `chrome_handle_app_action` wants.
                let pass = self.app_nav_pass(ui, animate, |app, ui, token, pass| {
                    // Paint an opaque panel background before the app draws.
                    // During an egui_nav slide the incoming route renders in a
                    // foreground layer over the outgoing one; the outgoing
                    // app's floating `egui::Area`s (popups, tooltips, side
                    // panels) escape egui_nav's route clip and would otherwise
                    // show through an app whose `render` doesn't fill its whole
                    // rect (e.g. Dave). This fill occludes whatever sits
                    // beneath the incoming route so every slide clips cleanly.
                    // It lives here rather than in the shared,
                    // business-logic-free `nav_frame` (columns paints its own
                    // column backgrounds and has its own nav path).
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        CornerRadius::ZERO,
                        ui.visuals().panel_fill,
                    );
                    app_ctx.navigator.set_pass(pass);
                    let resp = app.render_nav(app_ctx, ui, token);
                    app_ctx.navigator.set_pass(NavPass::Settled);
                    RouteResponse {
                        response: resp.action,
                        can_take_drag_from: resp.can_take_drag_from,
                    }
                });
                let app_action = pass.response;
                let can_take_drag_from = pass.can_take_drag_from;

                self.key_hints_ui(app_ctx, ui, app_rect);

                // Split borrows released — free the popped entry's resources by
                // routing the pop back to the app that owned it.
                self.cleanup_entries(app_ctx, pass.popped);

                // Retry an open raised on an earlier frame whose reference
                // hadn't resolved yet, before this frame's actions so a newer
                // unresolved open replaces it rather than the other way round.
                retry_pending_open(self, app_ctx, ui);

                // Route the bubbled app action.
                if let Some(action) = app_action {
                    chrome_handle_app_action(self, app_ctx, action, ui);
                }

                // Actions raised imperatively mid-render (e.g. a clicked inline
                // widget drawn by another app's KindRenderer) arrive here rather
                // than via the bubbled action; route them the same way.
                for action in app_ctx.app_actions.take() {
                    chrome_handle_app_action(self, app_ctx, action, ui);
                }

                RouteResponse {
                    response: None,
                    can_take_drag_from,
                }
            }
        });

        if let Some(action) = resp.action {
            if matches!(action, NavAction::Returned(_)) {
                self.nav.closed();
            } else if let NavAction::Navigating = action {
                self.nav.navigating = false;
            } else if let NavAction::Navigated = action {
                self.nav.opened();
            }
        }

        resp.drawer_response?
    }

    /// Show the side menu or bar, depending on if we're on a narrow
    /// or wide screen.
    ///
    /// The side menu should hover over the screen, while the side bar
    /// is collapsible but persistent on the screen.
    #[profiling::function]
    pub(super) fn show(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> Option<ChromePanelAction> {
        ui.spacing_mut().item_spacing.x = 0.0;

        let skb_anim =
            keyboard_visibility(ui, ctx, &mut self.options, &mut self.soft_kb_anim_state);

        let virtual_keyboard = self.options.contains(ChromeOptions::VirtualKeyboard);
        let keyboard_height = if self.options.contains(ChromeOptions::KeyboardVisibility) {
            skb_anim.anim_height
        } else {
            0.0
        };

        let is_narrow = notedeck::ui::is_narrow(ui.ctx());
        let toolbar_height = if is_narrow && ctx.settings.welcome_completed() {
            toolbar_visibility_height(skb_anim.skb_rect, ui)
        } else {
            0.0
        };

        // chrome-style app tab strip, desktop/wide only
        let show_app_tabs = !is_narrow && ctx.settings.welcome_completed();

        // Ctrl+Tab / Ctrl+Shift+Tab cycle through opened app tabs
        if show_app_tabs {
            let (mut next, mut prev) = (false, false);
            ui.input_mut(|i| {
                prev = i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Tab,
                );
                next = i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab);

                // macOS swallows Ctrl+Tab via AppKit's keyboard-interface
                // control before it reaches us, so also accept the native
                // Cmd+Shift+[ / Cmd+Shift+] tab-cycling shortcuts there. The
                // MAC_CMD modifier only matches when the Cmd key is set, which
                // only happens on macOS, so this is inert on other platforms.
                //
                // Because Shift is held, the logical key egui reports is the
                // shifted glyph: `{` (OpenCurlyBracket) and `}`
                // (CloseCurlyBracket), not the bare `[` / `]`.
                prev |= i.consume_key(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::OpenCurlyBracket,
                );
                next |= i.consume_key(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::CloseCurlyBracket,
                );
            });
            if prev {
                self.cycle_app(false);
            } else if next {
                self.cycle_app(true);
            }
        }

        // Back/forward via keyboard (Alt+←/→, mirroring the
        // Ctrl+Tab consume above) and the mouse's dedicated back/forward buttons
        // (PointerButton::Extra1/Extra2). Handled unconditionally so it works on
        // narrow layouts too, where the header chevrons aren't drawn. Consuming
        // the keys keeps a focused app from also acting on them.
        {
            let (mut back, mut forward) = (false, false);
            ui.input_mut(|i| {
                back = i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft);
                forward = i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight);
                back |= i.pointer.button_clicked(egui::PointerButton::Extra1);
                forward |= i.pointer.button_clicked(egui::PointerButton::Extra2);
            });
            if back {
                self.go_back(ctx);
            }
            if forward {
                self.go_forward();
            }
        }

        let (unseen_notifications, active_toolbar_tab) = if is_narrow {
            let unseen = self
                .get_columns_app()
                .map(|c| c.has_unseen_notifications(ctx.accounts))
                .unwrap_or(false);
            let active = self.active_toolbar_tab(ctx.accounts);
            (unseen, active)
        } else {
            (false, None)
        };

        // if the soft keyboard is open, shrink the chrome contents
        let mut action: Option<ChromePanelAction> = None;
        let mut toolbar_action: Option<ChromeToolbarAction> = None;
        // build a strip to carve out the soft keyboard inset
        let prev_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = 0.0;
        StripBuilder::new(ui)
            .size(Size::remainder())
            .size(Size::exact(toolbar_height))
            .size(Size::exact(keyboard_height))
            .vertical(|mut strip| {
                // the actual content, shifted up because of the soft keyboard
                strip.cell(|ui| {
                    ui.spacing_mut().item_spacing = prev_spacing;
                    if show_app_tabs {
                        chrome_app_tabs(self, ctx, ui);
                    }
                    // If the active app changed this frame (Ctrl+Tab above or a
                    // tab click in chrome_app_tabs), restore that app's last
                    // keyboard focus before rendering it, so the user can type
                    // immediately instead of having to click back in. Skipped on
                    // mobile to avoid popping the virtual keyboard on every switch.
                    if self.active != self.prev_active {
                        if !is_compiled_as_mobile() {
                            self.app_focus.restore(ui.ctx(), self.active as usize);
                        }
                        self.prev_active = self.active;
                    }
                    // The keymap reads the key press before the app renders,
                    // so a key it binds never reaches the app; focus has just
                    // been restored, so a refocused text field keeps its keys.
                    // The open command bar's query field takes every key.
                    if !self.command_bar.is_open() {
                        self.dispatch_keys(ctx, ui);
                    }
                    action = self.panel(ctx, ui, keyboard_height);
                });

                // mobile toolbar
                strip.cell(|ui| {
                    if toolbar_height > 0.0 {
                        toolbar_action =
                            chrome_toolbar(ui, unseen_notifications, active_toolbar_tab);
                    }
                });

                // the filler space taken up by the soft keyboard
                strip.cell(|ui| {
                    // keyboard-visibility virtual keyboard
                    if virtual_keyboard && keyboard_height > 0.0 {
                        virtual_keyboard_ui(ui, ui.available_rect_before_wrap())
                    }
                });
            });

        // hovering virtual keyboard
        if virtual_keyboard {
            if let Some(mut kb_rect) = skb_anim.skb_rect {
                let kb_height = if self.options.contains(ChromeOptions::KeyboardVisibility) {
                    keyboard_height
                } else {
                    400.0
                };
                kb_rect.min.y = kb_rect.max.y - kb_height;
                tracing::debug!("hovering virtual kb_height:{keyboard_height} kb_rect:{kb_rect}");
                virtual_keyboard_ui(ui, kb_rect)
            }
        }

        if let Some(tb_action) = toolbar_action {
            self.process_toolbar_action(tb_action, ctx);
        }

        // Remember what the app that rendered this frame has focused, to put
        // back when the user switches away and back. That's `prev_active`, not
        // `active`: an open or back raised mid-render has already moved
        // `active` on.
        // The open command bar has the focus; it gives the app back its own
        // as it closes.
        if !self.command_bar.is_open() {
            self.app_focus.remember(ui.ctx(), self.prev_active as usize);
        }

        action
    }
}

// Through a real chrome frame: an app whose `render_nav` records the
// `NavPass` it was told, slid forward and back by egui-nav. Release only, as
// `headway_nav_tests` is: mid-slide egui-nav draws two routes on two layers
// under one widget id, which egui debug-asserts against. CI's release nav
// step runs it.
#[cfg(all(test, not(feature = "auto-update"), not(debug_assertions)))]
mod nav_pass_tests {
    use super::Chrome;
    use crate::app::NotedeckApp;
    use notedeck::{App, AppContext, AppResponse, NavPass, NavRequest, Notedeck};
    use std::any::Any;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// One `render_nav` call: the `u32` token it drew (`None` for the root)
    /// and the pass the navigator named.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Drawn {
        token: Option<u32>,
        pass: NavPass,
    }

    /// An app that draws nothing and records each [`Drawn`].
    struct PassRecorder {
        drawn: Rc<RefCell<Vec<Drawn>>>,
    }

    impl App for PassRecorder {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn render_nav(
            &mut self,
            ctx: &mut AppContext<'_>,
            _ui: &mut egui::Ui,
            token: &Rc<dyn Any>,
        ) -> AppResponse {
            self.drawn.borrow_mut().push(Drawn {
                token: token.downcast_ref::<u32>().copied(),
                pass: ctx.navigator.pass(),
            });
            AppResponse::none()
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        notedeck: Notedeck,
        chrome: Chrome,
        drawn: Rc<RefCell<Vec<Drawn>>>,
        /// The navigator's pass once each frame's render returned.
        after: Vec<NavPass>,
    }

    fn frame(ui: &mut egui::Ui, rig: &mut Rig) {
        let mut ctx = rig.notedeck.app_context();
        rig.chrome.render(&mut ctx, ui);
        rig.after.push(ctx.navigator.pass());
    }

    /// Step frames until the stack is still, returning every draw on the
    /// way, in order.
    fn slide(harness: &mut egui_kittest::Harness<'_, Rig>) -> Vec<Drawn> {
        harness.state().drawn.borrow_mut().clear();
        for _ in 0..240 {
            harness.step();
            let nav = harness.state().chrome.active_nav().unwrap();
            if !nav.stack.navigating() && !nav.stack.returning() {
                break;
            }
        }
        // One frame still, to see the settled top.
        harness.step();
        harness.state().drawn.borrow().clone()
    }

    /// Each frame of a push's slide draws the root beneath, not landing, then
    /// the incoming top; a back's draws the root beneath, landing, then the
    /// outgoing top. Still, the top is drawn once, settled. The pass is
    /// put back to `Settled` once the app has drawn.
    #[test]
    fn render_nav_is_told_which_pass_it_draws() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = ["notedeck-test", "--testrunner"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let drawn = Rc::new(RefCell::new(Vec::new()));
        let recorder = PassRecorder {
            drawn: drawn.clone(),
        };
        let chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [NotedeckApp::Other("recorder".into(), Box::new(recorder))],
        );
        notedeck
            .app_context()
            .settings
            .get_settings_mut()
            .animate_nav_transitions = true;
        let rig = Rig {
            _dir: dir,
            notedeck,
            chrome,
            drawn,
            after: Vec::new(),
        };
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(frame, rig);
        harness.step();

        let root = |pass| Drawn { token: None, pass };
        let seven = |pass| Drawn {
            token: Some(7),
            pass,
        };

        let removed = harness
            .state_mut()
            .chrome
            .apply_nav_requests(vec![NavRequest::Push(Rc::new(7u32))]);
        assert!(removed.is_empty());
        let push = slide(&mut harness);
        let (still, sliding) = push.split_last().unwrap();
        assert_eq!(*still, seven(NavPass::Settled));
        assert!(sliding.len() >= 4, "the push slid: {push:?}");
        for pair in sliding.chunks(2) {
            assert_eq!(
                pair,
                [
                    root(NavPass::Beneath { lands: false }),
                    seven(NavPass::IncomingTop)
                ]
            );
        }

        let removed = harness
            .state_mut()
            .chrome
            .apply_nav_requests(vec![NavRequest::Back]);
        assert!(removed.is_empty());
        let back = slide(&mut harness);
        let (still, sliding) = back.split_last().unwrap();
        assert_eq!(*still, root(NavPass::Settled));
        assert!(sliding.len() >= 4, "the back slid: {back:?}");
        for pair in sliding.chunks(2) {
            assert_eq!(
                pair,
                [
                    root(NavPass::Beneath { lands: true }),
                    seven(NavPass::OutgoingTop)
                ]
            );
        }

        assert!(
            harness.state().after.iter().all(|&p| p == NavPass::Settled),
            "the pass is reset once the app has drawn"
        );
    }
}
