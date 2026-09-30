//! The narrow-layout mobile toolbar (Home, Chat, Search, Notifications): its
//! auto-hiding height, the tab bar itself, and routing a tapped tab to the
//! Columns / Messages app.

use super::Chrome;
use crate::app::NotedeckApp;
use egui::{Color32, Layout, Rect, Ui};
use notedeck::AppContext;

impl Chrome {
    pub(super) fn process_toolbar_action(
        &mut self,
        action: ChromeToolbarAction,
        ctx: &mut AppContext,
    ) {
        match action {
            ChromeToolbarAction::Home => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_home(ctx);
                }
            }
            #[cfg(feature = "messages")]
            ChromeToolbarAction::Chat => {
                self.switch_to_messages();
            }
            ChromeToolbarAction::Search => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_search(ctx);
                }
            }
            ChromeToolbarAction::Notifications => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_notifications(ctx);
                }
            }
        }
    }

    /// Returns which ChromeToolbarAction is currently "active" based on
    /// the active app and its route. Used to highlight the current tab.
    pub(super) fn active_toolbar_tab(
        &self,
        accounts: &notedeck::Accounts,
    ) -> Option<ChromeToolbarAction> {
        let active_app = &self.apps[self.active as usize];
        match active_app {
            #[cfg(feature = "messages")]
            NotedeckApp::Messages(_) => Some(ChromeToolbarAction::Chat),
            NotedeckApp::Columns(columns) => match columns.active_toolbar_tab(accounts) {
                Some(0) => Some(ChromeToolbarAction::Home),
                Some(1) => Some(ChromeToolbarAction::Search),
                Some(2) => Some(ChromeToolbarAction::Notifications),
                _ => None,
            },
            _ => None,
        }
    }
}

const TOOLBAR_HEIGHT: f32 = 48.0;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ChromeToolbarAction {
    Home,
    #[cfg(feature = "messages")]
    Chat,
    Search,
    Notifications,
}

/// Compute the animated toolbar height, auto-hiding on scroll and
/// when the soft keyboard is open.
pub(super) fn toolbar_visibility_height(skb_rect: Option<Rect>, ui: &mut Ui) -> f32 {
    let toolbar_visible_id = egui::Id::unique("chrome_toolbar_visible");

    let scroll_delta = scroll_delta(ui.ctx());
    let velocity_threshold = 1.0;

    if scroll_delta > velocity_threshold {
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, true));
    } else if scroll_delta < -velocity_threshold {
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, false));
    }

    let toolbar_visible = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(toolbar_visible_id))
        .unwrap_or(true);

    let toolbar_anim = ui
        .ctx()
        .animate_bool_responsive(toolbar_visible_id.with("anim"), toolbar_visible);

    if skb_rect.is_none() {
        TOOLBAR_HEIGHT * toolbar_anim
    } else {
        0.0
    }
}

/// Detect vertical scroll intent from mouse wheel, trackpad, or touch drag.
fn scroll_delta(ctx: &egui::Context) -> f32 {
    ctx.input(|i| {
        let sd = i.smooth_scroll_delta.y;
        if sd.abs() > 0.5 {
            return sd;
        }
        if i.pointer.is_decidedly_dragging() {
            return i.pointer.velocity().y;
        }
        0.0
    })
}

/// Render the Chrome mobile toolbar (Home, Chat, Search, Notifications).
pub(super) fn chrome_toolbar(
    ui: &mut Ui,
    unseen_notifications: bool,
    active_tab: Option<ChromeToolbarAction>,
) -> Option<ChromeToolbarAction> {
    use egui_tabs::{TabColor, Tabs};
    use notedeck_ui::icons::{home_button, notifications_button, search_button};

    let rect = ui.available_rect_before_wrap();
    let bg = if ui.visuals().dark_mode {
        Color32::BLACK
    } else {
        notedeck_ui::colors::ALMOST_WHITE
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );

    let has_chat = cfg!(feature = "messages");
    let mut next_index = 0;
    let home_index = next_index;
    next_index += 1;
    let chat_index = if has_chat {
        let i = next_index;
        next_index += 1;
        Some(i)
    } else {
        None
    };
    let search_index = next_index;
    next_index += 1;
    let notif_index = next_index;
    let tab_count = notif_index + 1;

    let rs = Tabs::new(tab_count)
        .selected(0)
        .hover_bg(TabColor::none())
        .selected_fg(TabColor::none())
        .selected_bg(TabColor::none())
        .height(TOOLBAR_HEIGHT)
        .layout(Layout::centered_and_justified(egui::Direction::TopDown))
        .show(ui, |ui, state| {
            let index = state.index();
            let btn_size: f32 = 20.0;

            if index == home_index {
                let active = active_tab == Some(ChromeToolbarAction::Home);
                if home_button(ui, btn_size, active).clicked() {
                    return Some(ChromeToolbarAction::Home);
                }
            } else if Some(index) == chat_index {
                #[cfg(feature = "messages")]
                {
                    let active = active_tab == Some(ChromeToolbarAction::Chat);
                    if notedeck_ui::icons::chat_button(ui, btn_size, active).clicked() {
                        return Some(ChromeToolbarAction::Chat);
                    }
                }
            } else if index == search_index {
                let active = active_tab == Some(ChromeToolbarAction::Search);
                if ui
                    .add(search_button(ui.visuals().text_color(), 2.0, active))
                    .clicked()
                {
                    return Some(ChromeToolbarAction::Search);
                }
            } else if index == notif_index {
                let active = active_tab == Some(ChromeToolbarAction::Notifications);
                if notifications_button(ui, btn_size, active, unseen_notifications).clicked() {
                    return Some(ChromeToolbarAction::Notifications);
                }
            }

            None
        })
        .inner();

    for maybe_r in rs {
        if maybe_r.inner.is_some() {
            return maybe_r.inner;
        }
    }

    None
}
