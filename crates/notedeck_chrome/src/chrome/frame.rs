//! The chrome's per-frame layout: the side drawer over the global-nav frame
//! that renders the active app, with the desktop tab strip above it and the
//! mobile toolbar and soft-keyboard inset below.

use super::actions::chrome_handle_app_action;
use super::header::chrome_app_tabs;
use super::keyboard::{keyboard_visibility, virtual_keyboard_ui};
use super::sidebar::{milestone_name, topdown_sidebar, SidebarOptions};
use super::toolbar::{chrome_toolbar, toolbar_visibility_height, ChromeToolbarAction};
use super::{Chrome, ChromePanelAction, ChromeRoute};
use crate::ChromeOptions;
use egui::{CornerRadius, Layout, Margin};
use egui_extras::{Size, StripBuilder};
use egui_nav::{NavAction, NavDrawer, NavUiType, RouteResponse};
use notedeck::ui::is_compiled_as_mobile;
use notedeck::{nav_frame, App, AppContext, NavStackEvent};

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

                // `nav_frame` needs `&mut global_nav` while the render callback
                // needs `&mut apps` — split-borrow the two fields so both can be
                // lent at once. The app's action is bubbled out via the frame
                // response and routed *after* `nav_frame` returns, since the
                // callback can't reborrow the whole `self` that
                // `chrome_handle_app_action` wants.
                let (app_action, can_take_drag_from, popped) = {
                    let Chrome {
                        global_nav, apps, ..
                    } = &mut *self;
                    let nav = global_nav
                        .as_mut()
                        .expect("global nav is seeded at construction");

                    let frame = nav_frame(
                        ui,
                        egui::Id::new("chrome_global_nav"),
                        nav,
                        animate,
                        |ui, ui_type, frame_nav| match ui_type {
                            NavUiType::Body => {
                                // Paint an opaque panel background before the app
                                // draws. During an egui_nav slide the incoming
                                // route renders in a foreground layer over the
                                // outgoing one; the outgoing app's floating
                                // `egui::Area`s (popups, tooltips, side panels)
                                // escape egui_nav's route clip and would otherwise
                                // show through an app whose `render` doesn't fill
                                // its whole rect (e.g. Dave). This fill occludes
                                // whatever sits beneath the incoming route so every
                                // app switch clips cleanly. It lives here rather
                                // than in the shared, business-logic-free
                                // `nav_frame` (columns paints its own column
                                // backgrounds and has its own nav path).
                                ui.painter().rect_filled(
                                    ui.max_rect(),
                                    CornerRadius::ZERO,
                                    ui.visuals().panel_fill,
                                );
                                let entry =
                                    frame_nav.routes().last().expect("stack always has a top");
                                let resp =
                                    apps[entry.app.slot()].render_nav(app_ctx, ui, &entry.token);
                                RouteResponse {
                                    response: resp.action,
                                    can_take_drag_from: resp.can_take_drag_from,
                                }
                            }

                            // The global-nav header (back button, breadcrumb)
                            // lands with the nav controls in a later subissue.
                            NavUiType::Title => RouteResponse {
                                response: None,
                                can_take_drag_from: Vec::new(),
                            },
                        },
                    );

                    // A completed global-back popped the top entry inside
                    // `nav_frame`. Surface the popped entry (its owning app + the
                    // opaque route token) so we can hand it to that app's
                    // `cleanup_nav` once the split borrows above are released —
                    // the chrome itself never inspects the token. The token is an
                    // `Rc`, so cloning it out is a refcount bump.
                    let popped = match frame.event {
                        Some(NavStackEvent::Popped {
                            route: Some(entry), ..
                        }) => Some((entry.app, entry.token)),
                        _ => None,
                    };

                    (frame.response, frame.can_take_drag_from, popped)
                };

                // Split borrows released — free the popped entry's resources by
                // routing the pop back to the app that owned it (e.g. columns
                // closes a deep-linked thread's subscription).
                if let Some((app, token)) = popped {
                    self.apps[app.slot()].cleanup_nav(app_ctx, &token);
                }

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

                // A completed back may have popped the top inside `nav_frame`;
                // re-derive the active app so it tracks the new top.
                self.sync_active_from_nav();

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

        // Global history back/forward via keyboard (Alt+←/→, mirroring the
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
                self.global_go_back();
            }
            if forward {
                self.global_go_forward();
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
                            if let Some(id) = self.app_focus.get(&(self.active as usize)) {
                                ui.ctx().memory_mut(|m| m.request_focus(*id));
                            } else {
                                // First activation, nothing remembered: ask the
                                // app's autofocus widget (if any) to grab focus,
                                // like browser autofocus.
                                notedeck_ui::request_autofocus(ui.ctx());
                            }
                        }
                        self.prev_active = self.active;
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

        // Remember the active app's currently-focused widget so we can restore
        // it when the user switches away and back. Only overwrite on Some so the
        // last real focus is retained even after focus is transiently dropped.
        if let Some(focused) = ui.ctx().memory(|m| m.focused()) {
            self.app_focus.insert(self.active as usize, focused);
        }

        action
    }
}
