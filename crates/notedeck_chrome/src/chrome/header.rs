//! The desktop header strip across the top of the chrome frame: the
//! browser-style global-history controls (history dropdown, back / forward
//! chevrons) followed by one tab per opened app.

use super::sidebar::app_label;
#[cfg(feature = "dave")]
use super::sidebar::dave_button;
use super::Chrome;
use crate::app::NotedeckApp;
use egui::{vec2, Color32, Label, Layout, Rect, RichText, Sense};
use notedeck::{App, AppContext, Localization, NotedeckOptions, TabNotifications};
use notedeck_ui::app_images;
use notedeck_ui::header::{paint_chevron, ChevronDir};
use oot_bitset::bitset_get;

/// Render a small (`size` square) app icon for the chrome tab strip.
fn tab_app_icon(ui: &mut egui::Ui, app: &mut NotedeckApp, size: f32) {
    match app {
        NotedeckApp::Columns(_) => {
            ui.add(app_images::columns_image().max_width(size).max_height(size));
        }

        #[cfg(feature = "dave")]
        NotedeckApp::Dave(dave) => {
            let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
            dave_button(dave.avatar_mut(), ui, rect);
        }

        #[cfg(feature = "dashboard")]
        NotedeckApp::Dashboard(_) => {
            notedeck_ui::icons::dashboard_icon(ui, size);
        }

        #[cfg(feature = "horizon")]
        NotedeckApp::Horizon(_) => {
            notedeck_ui::icons::horizon_icon(ui, size);
        }

        #[cfg(feature = "messages")]
        NotedeckApp::Messages(_) => {
            notedeck_ui::icons::messages_icon(ui, size);
        }

        #[cfg(feature = "clndash")]
        NotedeckApp::ClnDash(_) => {
            ui.add(app_images::cln_image().max_width(size).max_height(size));
        }

        #[cfg(feature = "notebook")]
        NotedeckApp::Notebook(_) => {
            notedeck_ui::icons::notebook_icon(ui, size);
        }

        #[cfg(feature = "headway")]
        NotedeckApp::Headway(_) => {
            notedeck_ui::icons::headway_icon(ui, size);
        }

        #[cfg(feature = "nostrverse")]
        NotedeckApp::Nostrverse(_) => {
            ui.add(
                app_images::universe_image()
                    .max_width(size)
                    .max_height(size),
            );
        }

        NotedeckApp::Other(_name, _) => {
            ui.label("W");
        }
    }
}

/// The app index of the `n`th opened app (the app backing the `n`th tab).
fn nth_opened(opened: &[u16], app_count: usize, n: usize) -> Option<usize> {
    (0..app_count)
        .filter(|&i| bitset_get(opened, i as u16))
        .nth(n)
}

/// A subtle separator beneath the tab strip: a faint line that softens into a
/// short gradient fading up into the tab strip, instead of a hard hairline.
fn tab_strip_fade(ui: &egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    let top = rect.top();
    let fade_height = 6.0;
    let (left, right) = (rect.left(), rect.right());

    let base = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let line = base.gamma_multiply(0.6);
    let transparent = Color32::TRANSPARENT;

    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(egui::pos2(left, top), line);
    mesh.colored_vertex(egui::pos2(right, top), line);
    mesh.colored_vertex(egui::pos2(right, top - fade_height), transparent);
    mesh.colored_vertex(egui::pos2(left, top - fade_height), transparent);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// A single tab in the chrome app tab strip: the app it represents, whether
/// it's the active tab, and any notification badge it wants to show.
struct ChromeTab<'a> {
    app: &'a mut NotedeckApp,
    selected: bool,
    notifications: TabNotifications,
}

impl ChromeTab<'_> {
    fn show(&mut self, loc: &mut Localization, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            tab_app_icon(ui, self.app, 18.0);

            let txt = RichText::new(app_label(loc, self.app));
            let txt = if self.selected {
                txt
            } else {
                txt.color(ui.visuals().weak_text_color())
            };
            ui.add(Label::new(txt).selectable(false));

            tab_notification_badge(ui, self.notifications);
        });
    }
}

/// Render a small pill badge with the notification count, if any.
fn tab_notification_badge(ui: &mut egui::Ui, notifs: TabNotifications) {
    if notifs.is_empty() {
        return;
    }

    let label = if notifs.count > 99 {
        "99+".to_owned()
    } else {
        notifs.count.to_string()
    };

    let galley =
        ui.painter()
            .layout_no_wrap(label, egui::FontId::proportional(11.0), Color32::WHITE);

    let padding = vec2(5.0, 1.0);
    let size = galley.size() + padding * 2.0;
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, rect.height() / 2.0, notedeck_ui::colors::PINK);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// Chrome-browser-style tab strip across the top of the chrome frame. Shows one
/// tab per *opened* app (`Chrome::opened`); clicking a tab switches the active
/// app. New apps are opened from the left sidebar.
/// Horizontal space (logical points) to reserve at the left of the tab strip so
/// it isn't obscured by the macOS traffic-light window controls. These are only
/// drawn when we hide the native titlebar (the default, fullsize-content-view
/// mode); with `--title` the native titlebar owns the controls and no reserve is
/// needed. Returns 0 on other platforms, when the titlebar is shown, or in
/// fullscreen (where the controls are hidden).
fn macos_traffic_light_inset(ctx: &AppContext, ui: &egui::Ui) -> f32 {
    if cfg!(target_os = "macos")
        && !ctx.args.options.contains(NotedeckOptions::ShowTitle)
        && ui.input(|i| i.viewport().fullscreen) != Some(true)
    {
        72.0
    } else {
        0.0
    }
}

/// Fixed slot size for a header nav button (history clock, back/forward chevron).
const NAV_BTN_SIZE: egui::Vec2 = vec2(28.0, 30.0);

/// Browser-style global-history controls drawn at the left of the chrome tab
/// strip (matching the reference screenshot): a history dropdown (clock) listing
/// the recent global-stack entries by title, then back / forward chevrons. The
/// chevrons grey out when the stack can't move that way
/// ([`NavStack::can_go_back`](notedeck::NavStack::can_go_back) /
/// [`can_go_forward`](notedeck::NavStack::can_go_forward)). Clicks drive the
/// global stack through [`Chrome::global_go_back`]/`global_go_forward`/`global_go_to`,
/// exactly like the `Alt+←/→` keyboard and mouse-button shortcuts.
fn chrome_nav_controls(chrome: &mut Chrome, ctx: &mut AppContext, ui: &mut egui::Ui) {
    let (can_back, can_forward) = chrome
        .global_nav
        .as_ref()
        .map(|nav| (nav.can_go_back(), nav.can_go_forward()))
        .unwrap_or((false, false));

    // History dropdown (clock). Its list is built lazily, only while open.
    let (clock_resp, clock_rect) = nav_button_slot(ui, true);
    paint_clock(
        ui.painter(),
        clock_rect.center(),
        7.0,
        ui.visuals().text_color(),
    );
    // A click on the clock toggles the list; picking an entry closes it.
    let mut jump_to: Option<usize> = None;
    egui::Popup::from_toggle_button_response(&clock_resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(220.0);
            let Some(nav) = chrome.global_nav.as_ref() else {
                return;
            };
            let routes = nav.routes();
            let current = routes.len() - 1;
            // Newest (current) entry first, like a browser history menu. Each
            // entry's title comes from the app that owns it (falling back to the
            // app label until apps push per-view tokens); clicking an older one
            // jumps straight to it.
            for (i, entry) in routes.iter().enumerate().rev() {
                let Some(app) = chrome.apps.get(entry.app.slot()) else {
                    continue;
                };
                let title = app
                    .nav_title(&entry.token)
                    .unwrap_or_else(|| app_label(ctx.i18n, app));
                if ui.selectable_label(i == current, title).clicked() && i != current {
                    jump_to = Some(i);
                }
            }
        });
    if let Some(index) = jump_to {
        chrome.global_go_to(index);
    }

    ui.add_space(4.0);

    // Back / forward chevrons, greyed when the stack can't move that way.
    let (back_resp, back_rect) = nav_button_slot(ui, can_back);
    paint_nav_chevron(
        ui.painter(),
        back_rect,
        ChevronDir::Left,
        chevron_color(ui, can_back),
    );
    if back_resp.clicked() {
        chrome.global_go_back();
    }

    let (fwd_resp, fwd_rect) = nav_button_slot(ui, can_forward);
    paint_nav_chevron(
        ui.painter(),
        fwd_rect,
        ChevronDir::Right,
        chevron_color(ui, can_forward),
    );
    if fwd_resp.clicked() {
        chrome.global_go_forward();
    }

    // A little breathing room before the tabs.
    ui.add_space(8.0);
}

/// Allocate a fixed [`NAV_BTN_SIZE`] slot for a header nav button, painting a
/// subtle rounded hover background when `enabled` and hovered. Returns the
/// response and the slot rect for the caller to paint its glyph into. A disabled
/// slot only senses hover, so its clicks are inert (a greyed-out control).
fn nav_button_slot(ui: &mut egui::Ui, enabled: bool) -> (egui::Response, Rect) {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(NAV_BTN_SIZE, sense);
    if !enabled {
        return (response, rect);
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink(3.0),
            6.0,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    }
    (response, rect)
}

/// Colour for a header chevron: normal when the control is enabled, greyed (per
/// the reference screenshot) when the stack can't move that way.
fn chevron_color(ui: &egui::Ui, enabled: bool) -> Color32 {
    if enabled {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    }
}

/// Paint a back/forward chevron centred in `rect`.
fn paint_nav_chevron(painter: &egui::Painter, rect: Rect, dir: ChevronDir, color: Color32) {
    let glyph = Rect::from_center_size(rect.center(), vec2(10.0, 13.0));
    paint_chevron(painter, glyph, 2.0, dir, egui::Stroke::new(1.6_f32, color));
}

/// Paint a small clock glyph (ring + two hands) centred at `center`, used for the
/// history dropdown button.
fn paint_clock(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let stroke = egui::Stroke::new(1.5_f32, color);
    painter.circle_stroke(center, radius, stroke);
    // Hands reading ~10:10 — hour hand up, minute hand to the right.
    painter.line_segment([center, center - vec2(0.0, radius * 0.55)], stroke);
    painter.line_segment([center, center + vec2(radius * 0.6, 0.0)], stroke);
}

pub(super) fn chrome_app_tabs(chrome: &mut Chrome, ctx: &mut AppContext, ui: &mut egui::Ui) {
    let inset = macos_traffic_light_inset(ctx, ui);

    let n_apps = chrome.apps.len();

    // the active app is always opened, so there is always at least one tab
    let n_tabs = (0..n_apps)
        .filter(|&i| bitset_get(&chrome.opened, i as u16))
        .count();
    if n_tabs == 0 {
        return;
    }

    // the selected tab is the number of opened apps before the active app
    let active = chrome.active.max(0) as usize;
    let sel = (0..active.min(n_apps))
        .filter(|&i| bitset_get(&chrome.opened, i as u16))
        .count();

    ui.spacing_mut().item_spacing.y = 0.0;

    // The header strip is one horizontal row: the macOS traffic-light inset, the
    // browser-style history controls (back/forward + dropdown), then the tabs.
    let tab_res = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // On macOS with the native titlebar hidden, indent so the
            // traffic-light window controls don't sit on top of the controls.
            if inset > 0.0 {
                ui.add_space(inset);
            }

            chrome_nav_controls(chrome, ctx, ui);

            // disjoint field borrows so the tab closure can read opened flags
            // while mutably rendering app icons (e.g. Dave's avatar)
            let opened = &chrome.opened;
            let apps = &mut chrome.apps;

            // egui_tabs keys its selection on `ui.id().with("tabs")` and prefers
            // that temp value over `.selected()`, so overwrite it each frame to
            // stay in sync with app switches that happen elsewhere (Ctrl+Tab, the
            // sidebar, note actions). It must be keyed off the *same* ui we hand
            // to `Tabs::show`.
            let tabs_id = ui.scope_id().with("tabs");
            ui.ctx().data_mut(|d| d.insert_temp(tabs_id, sel as i32));

            egui_tabs::Tabs::new(n_tabs as i32)
                .selected(sel as i32)
                .hover_bg(egui_tabs::TabColor::none())
                .selected_fg(egui_tabs::TabColor::none())
                .selected_bg(egui_tabs::TabColor::none())
                .height(30.0)
                .layout(Layout::centered_and_justified(egui::Direction::TopDown))
                .show(ui, |ui, state| {
                    let Some(app_idx) = nth_opened(opened, n_apps, state.index() as usize) else {
                        return;
                    };

                    let notifications = apps[app_idx].tab_notifications(ctx);
                    ChromeTab {
                        app: &mut apps[app_idx],
                        selected: state.is_selected(),
                        notifications,
                    }
                    .show(ctx.i18n, ui);
                })
        })
        .inner;

    tab_strip_fade(ui);

    // Switch on an actual click this frame rather than on the strip's current
    // selection. Reading the selection back would make the tab strip fight
    // every app switch that originates elsewhere (Ctrl+Tab, the sidebar, a
    // note action opening Headway) instead of merely reflecting it.
    let Some(clicked_tab) = tab_res.inner().iter().position(|r| r.response.clicked()) else {
        return;
    };

    let Some(app_idx) = nth_opened(&chrome.opened, chrome.apps.len(), clicked_tab) else {
        return;
    };

    chrome.set_active(app_idx as i32);
}
