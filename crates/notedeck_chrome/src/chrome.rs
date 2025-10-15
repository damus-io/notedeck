// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use bitflags::bitflags;
use eframe::CreationContext;
use egui::{
    vec2, Button, Color32, CornerRadius, Label, Layout, Rect, RichText, ThemePreference, Widget,
};
use egui_extras::{Size, StripBuilder};
use egui_nav::RouteResponse;
use egui_nav::{NavAction, NavDrawer};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::AppResponse;
use notedeck::DrawerRouter;
use notedeck::Error;
use notedeck::SoftKeyboardContext;
use notedeck::{
    tr, App, AppAction, AppContext, Localization, Notedeck, NotedeckOptions, NotedeckTextStyle,
    UserAccount, WalletType,
};
use notedeck_calendar::CalendarApp;
use notedeck_columns::{timeline::TimelineKind, Damus};
use notedeck_dave::{Dave, DaveAvatar};
use notedeck_ui::{
    app_images, expanding_button, AnimationHelper, ProfilePic, ICON_EXPANSION_MULTIPLE, ICON_WIDTH,
};
use std::collections::HashMap;

#[derive(Default)]
pub struct Chrome {
    active: i32,
    options: ChromeOptions,
    apps: Vec<NotedeckApp>,

    /// The state of the soft keyboard animation
    soft_kb_anim_state: AnimState,

    pub repaint_causes: HashMap<egui::RepaintCause, u64>,
    nav: DrawerRouter,
}

#[derive(Clone)]
enum ChromeRoute {
    Chrome,
    App,
}

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    Wallet,
    SaveTheme(ThemePreference),
    Profile(notedeck::enostr::Pubkey),
}

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SidebarOptions: u8 {
        const Compact = 1 << 0;
    }
}

impl ChromePanelAction {
    fn columns_navigate(ctx: &mut AppContext, chrome: &mut Chrome, route: notedeck_columns::Route) {
        chrome.switch_to_columns();

        if let Some(c) = chrome.get_columns_app().and_then(|columns| {
            columns
                .decks_cache
                .selected_column_mut(ctx.i18n, ctx.accounts)
        }) {
            if c.router().routes().iter().any(|r| r == &route) {
                // return if we are already routing to accounts
                c.router_mut().go_back();
            } else {
                c.router_mut().route_to(route);
                //c..route_to(Route::relays());
            }
        };
    }

    #[profiling::function]
    fn process(&self, ctx: &mut AppContext, chrome: &mut Chrome, ui: &mut egui::Ui) {
        match self {
            Self::SaveTheme(theme) => {
                ui.ctx().set_theme(*theme);
                ctx.settings.set_theme(*theme);
            }

            Self::Support => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Support);
            }

            Self::Account => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::accounts());
            }

            Self::Settings => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Settings);
            }

            Self::Wallet => {
                Self::columns_navigate(
                    ctx,
                    chrome,
                    notedeck_columns::Route::Wallet(WalletType::Auto),
                );
            }
            Self::Profile(pk) => {
                columns_route_to_profile(pk, chrome, ctx, ui);
            }
        }
    }
}

/// Some people have been running notedeck in debug, let's catch that!
fn stop_debug_mode(options: NotedeckOptions) {
    if !options.contains(NotedeckOptions::Tests)
        && cfg!(debug_assertions)
        && !options.contains(NotedeckOptions::Debug)
    {
        println!("--- WELCOME TO DAMUS NOTEDECK! ---");
        println!(
            "It looks like are running notedeck in debug mode, unless you are a developer, this is not likely what you want."
        );
        println!("If you are a developer, run `cargo run -- --debug` to skip this message.");
        println!("For everyone else, try again with `cargo run --release`. Enjoy!");
        println!("---------------------------------");
        panic!();
    }
}

impl Chrome {
    /// Create a new chrome with the default app setup
    pub fn new_with_apps(
        cc: &CreationContext,
        app_args: &[String],
        notedeck: &mut Notedeck,
    ) -> Result<Self, Error> {
        stop_debug_mode(notedeck.options());

        let context = &mut notedeck.app_context();
        let dave = Dave::new(cc.wgpu_render_state.as_ref());
        let columns = Damus::new(context, app_args);
        let calendar = CalendarApp::new();
        let mut chrome = Chrome::default();

        notedeck.check_args(columns.unrecognized_args())?;

        chrome.add_app(NotedeckApp::Columns(Box::new(columns)));
        chrome.add_app(NotedeckApp::Calendar(Box::new(calendar)));
        chrome.add_app(NotedeckApp::Dave(Box::new(dave)));

        if notedeck.has_option(NotedeckOptions::FeatureNotebook) {
            chrome.add_app(NotedeckApp::Notebook(Box::default()));
        }

        if notedeck.has_option(NotedeckOptions::FeatureClnDash) {
            chrome.add_app(NotedeckApp::ClnDash(Box::default()));
        }

        chrome.set_active(0);

        Ok(chrome)
    }

    pub fn toggle(&mut self) {
        if self.nav.drawer_focused {
            self.nav.close();
        } else {
            self.nav.open();
        }
    }

    pub fn add_app(&mut self, app: NotedeckApp) {
        self.apps.push(app);
    }

    fn get_columns_app(&mut self) -> Option<&mut Damus> {
        for app in &mut self.apps {
            if let NotedeckApp::Columns(cols) = app {
                return Some(cols);
            }
        }

        None
    }

    fn switch_to_columns(&mut self) {
        for (i, app) in self.apps.iter().enumerate() {
            if let NotedeckApp::Columns(_) = app {
                self.active = i as i32;
            }
        }
    }

    pub fn set_active(&mut self, app: i32) {
        self.active = app;
    }

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
            .opened_offset(100.0);

        let resp = drawer.show_mut(ui, |ui, route| match route {
            ChromeRoute::Chrome => {
                ui.painter().rect_filled(
                    ui.available_rect_before_wrap(),
                    CornerRadius::ZERO,
                    ui.visuals().panel_fill,
                );
                _ = ui.vertical_centered(|ui| {
                    self.topdown_sidebar(ui, app_ctx.i18n);
                });

                ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                    let options = if amt_keyboard_open > 0.0 {
                        SidebarOptions::Compact
                    } else {
                        SidebarOptions::default()
                    };
                    let response = bottomup_sidebar(self, app_ctx, ui, options);

                    RouteResponse {
                        response,
                        can_take_drag_from: Vec::new(),
                    }
                })
                .inner
            }
            ChromeRoute::App => {
                let resp = self.apps[self.active as usize].update(app_ctx, ui);

                if let Some(action) = resp.action {
                    chrome_handle_app_action(self, app_ctx, action, ui);
                }

                RouteResponse {
                    response: None,
                    can_take_drag_from: resp.can_take_drag_from,
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
    fn show(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<ChromePanelAction> {
        ui.spacing_mut().item_spacing.x = 0.0;

        let skb_anim =
            keyboard_visibility(ui, ctx, &mut self.options, &mut self.soft_kb_anim_state);

        let virtual_keyboard = self.options.contains(ChromeOptions::VirtualKeyboard);
        let keyboard_height = if self.options.contains(ChromeOptions::KeyboardVisibility) {
            skb_anim.anim_height
        } else {
            0.0
        };

        // if the soft keyboard is open, shrink the chrome contents
        let mut action: Option<ChromePanelAction> = None;
        // build a strip to carve out the soft keyboard inset
        StripBuilder::new(ui)
            .size(Size::remainder())
            .size(Size::exact(keyboard_height))
            .vertical(|mut strip| {
                // the actual content, shifted up because of the soft keyboard
                strip.cell(|ui| {
                    action = self.panel(ctx, ui, keyboard_height);
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

        action
    }

    fn topdown_sidebar(&mut self, ui: &mut egui::Ui, i18n: &mut Localization) {
        // macos needs a bit of space to make room for window
        // minimize/close buttons
        if cfg!(target_os = "macos") {
            ui.add_space(30.0);
        } else {
            // we still want *some* padding so that it aligns with the + button regardless
            ui.add_space(notedeck_ui::constants::FRAME_MARGIN.into());
        }

        if ui.add(expand_side_panel_button()).clicked() {
            self.nav.close();
        }

        ui.add_space(4.0);
        ui.add(milestone_name(i18n));
        //let dark_mode = ui.ctx().style().visuals.dark_mode;

        for (i, app) in self.apps.iter_mut().enumerate() {
            let is_selected = self.active == i as i32;
            let r = match app {
                NotedeckApp::Columns(_columns_app) => columns_button(ui),

                NotedeckApp::Dave(dave) => {
                    ui.add_space(24.0);
                    let rect = dave_sidebar_rect(ui);
                    dave_button(dave.avatar_mut(), ui, rect)
                }
                NotedeckApp::Calendar(_calendar) => calendar_button(ui, is_selected),

                NotedeckApp::ClnDash(_clndash) => clndash_button(ui),

                NotedeckApp::Notebook(_notebook) => notebook_button(ui),

                NotedeckApp::Other(_other) => {
                    // app provides its own button rendering ui?
                    panic!("TODO: implement other apps")
                }
            };

            ui.add_space(4.0);

            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.active = i as i32;
                self.nav.close();
            }
        }
    }
}

impl notedeck::App for Chrome {
    fn update(&mut self, ctx: &mut notedeck::AppContext, ui: &mut egui::Ui) -> AppResponse {
        if let Some(action) = self.show(ctx, ui) {
            action.process(ctx, self, ui);
            self.nav.close();
        }
        // TODO: unify this constant with the columns side panel width. ui crate?
        AppResponse::none()
    }
}

fn milestone_name<'a>(i18n: &'a mut Localization) -> impl Widget + 'a {
    |ui: &mut egui::Ui| -> egui::Response {
        ui.vertical_centered(|ui| {
            let font = egui::FontId::new(
                notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Tiny),
                egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(
                Label::new(
                    RichText::new(tr!(i18n, "BETA", "Beta version label"))
                        .color(ui.style().visuals.noninteractive().fg_stroke.color)
                        .font(font),
                )
                .selectable(false),
            )
            .on_hover_text(tr!(
                i18n,
                "Notedeck is a beta product. Expect bugs and contact us when you run into issues.",
                "Beta product warning message"
            ))
            .on_hover_cursor(egui::CursorIcon::Help)
        })
        .inner
    }
}

fn expand_side_panel_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 40.0;
        let img = app_images::damus_image()
            .max_width(img_size)
            .sense(egui::Sense::click());

        ui.add(img)
    }
}

fn support_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "help-button",
        16.0,
        app_images::help_light_image(),
        app_images::help_dark_image(),
        ui,
        false,
    )
}

fn settings_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "settings-button",
        32.0,
        app_images::settings_light_image(),
        app_images::settings_dark_image(),
        ui,
        false,
    )
}

fn columns_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "columns-button",
        40.0,
        app_images::columns_image(),
        app_images::columns_image(),
        ui,
        false,
    )
}

fn calendar_button(ui: &mut egui::Ui, selected: bool) -> egui::Response {
    let size = vec2(60.0, 60.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let rounding = CornerRadius::same(12);
        let fill = if selected {
            ui.visuals().selection.bg_fill
        } else {
            visuals.bg_fill
        };
        let stroke = if selected {
            ui.visuals().selection.stroke
        } else {
            visuals.bg_stroke
        };
        let painter = ui.painter();
        painter.rect(rect, rounding, fill, stroke, egui::StrokeKind::Middle);

        let header_rect = egui::Rect::from_min_max(
            rect.min + vec2(8.0, 8.0),
            egui::pos2(rect.max.x - 8.0, rect.min.y + 8.0 + size.y * 0.25),
        );
        let header_rounding = CornerRadius {
            nw: rounding.nw,
            ne: rounding.ne,
            sw: 6,
            se: 6,
        };
        let header_fill = ui.visuals().widgets.active.bg_fill;
        painter.rect_filled(header_rect, header_rounding, header_fill);

        let binding_color = ui.visuals().text_color();
        let binding_radius = 3.5;
        let binding_y = header_rect.top() - binding_radius - 2.0;
        for x in [header_rect.left() + 10.0, header_rect.right() - 10.0] {
            painter.circle_filled(egui::pos2(x, binding_y), binding_radius, binding_color);
        }

        let divider_color = ui.visuals().widgets.noninteractive.fg_stroke.color;
        painter.line_segment(
            [
                egui::pos2(header_rect.left(), header_rect.bottom()),
                egui::pos2(header_rect.right(), header_rect.bottom()),
            ],
            egui::Stroke::new(1.0, divider_color),
        );

        let grid_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + 10.0, header_rect.bottom() + 8.0),
            egui::pos2(rect.max.x - 10.0, rect.max.y - 12.0),
        );
        let grid_stroke = egui::Stroke::new(1.3, ui.visuals().text_color());

        for row in 1..3 {
            let y = egui::lerp(grid_rect.y_range(), row as f32 / 3.0);
            painter.line_segment(
                [
                    egui::pos2(grid_rect.min.x, y),
                    egui::pos2(grid_rect.max.x, y),
                ],
                grid_stroke,
            );
        }

        for col in 1..2 {
            let x = egui::lerp(grid_rect.x_range(), col as f32 / 2.0);
            painter.line_segment(
                [
                    egui::pos2(x, grid_rect.min.y),
                    egui::pos2(x, grid_rect.max.y),
                ],
                grid_stroke,
            );
        }
    }
    response
}

fn accounts_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "accounts-button",
        24.0,
        app_images::accounts_image().tint(ui.visuals().text_color()),
        app_images::accounts_image(),
        ui,
        false,
    )
}

fn clndash_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "clndash-button",
        24.0,
        app_images::cln_image(),
        app_images::cln_image(),
        ui,
        false,
    )
}

fn notebook_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "notebook-button",
        40.0,
        app_images::algo_image(),
        app_images::algo_image(),
        ui,
        false,
    )
}

fn dave_sidebar_rect(ui: &mut egui::Ui) -> Rect {
    let size = vec2(60.0, 60.0);
    let available = ui.available_rect_before_wrap();
    let center_x = available.center().x;
    let center_y = available.top();
    egui::Rect::from_center_size(egui::pos2(center_x, center_y), size)
}

fn dave_button(avatar: Option<&mut DaveAvatar>, ui: &mut egui::Ui, rect: Rect) -> egui::Response {
    if let Some(avatar) = avatar {
        avatar.render(rect, ui)
    } else {
        // plain icon if wgpu device not available??
        ui.label("fixme")
    }
}

pub fn get_profile_url_owned(profile: Option<ProfileRecord<'_>>) -> &str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        notedeck::profile::no_pfp_url()
    }
}

pub fn get_account_url<'a>(
    txn: &'a nostrdb::Transaction,
    ndb: &nostrdb::Ndb,
    account: &UserAccount,
) -> &'a str {
    if let Ok(profile) = ndb.get_profile_by_pubkey(txn, account.key.pubkey.bytes()) {
        get_profile_url_owned(Some(profile))
    } else {
        get_profile_url_owned(None)
    }
}

fn wallet_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 24.0;

        let max_size = img_size * ICON_EXPANSION_MULTIPLE;

        let img = if !ui.visuals().dark_mode {
            app_images::wallet_light_image()
        } else {
            app_images::wallet_dark_image()
        }
        .max_width(img_size);

        let helper = AnimationHelper::new(ui, "wallet-icon", vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper.take_animation_response()
    }
}

fn chrome_handle_app_action(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    action: AppAction,
    ui: &mut egui::Ui,
) {
    match action {
        AppAction::ToggleChrome => {
            chrome.toggle();
        }
        AppAction::ShowColumns => {
            chrome.switch_to_columns();
        }

        AppAction::Note(note_action) => {
            chrome.switch_to_columns();
            let Some(columns) = chrome.get_columns_app() else {
                return;
            };

            let txn = Transaction::new(ctx.ndb).unwrap();

            let cols = columns
                .decks_cache
                .active_columns_mut(ctx.i18n, ctx.accounts)
                .unwrap();
            let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
                note_action,
                ctx.ndb,
                cols,
                0,
                &mut columns.timeline_cache,
                &mut columns.threads,
                ctx.note_cache,
                ctx.pool,
                &txn,
                ctx.unknown_ids,
                ctx.accounts,
                ctx.global_wallet,
                ctx.zaps,
                ctx.img_cache,
                &mut columns.view_state,
                ui,
            );

            if let Some(action) = m_action {
                let col = cols.selected_mut();

                action.process(&mut col.router, &mut col.sheet_router);
            }
        }
    }
}

fn columns_route_to_profile(
    pk: &notedeck::enostr::Pubkey,
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    chrome.switch_to_columns();
    let Some(columns) = chrome.get_columns_app() else {
        return;
    };

    let cols = columns
        .decks_cache
        .active_columns_mut(ctx.i18n, ctx.accounts)
        .unwrap();

    let router = cols.get_selected_router();
    if router.routes().iter().any(|r| {
        matches!(
            r,
            notedeck_columns::Route::Timeline(TimelineKind::Profile(_))
        )
    }) {
        router.go_back();
        return;
    }

    let txn = Transaction::new(ctx.ndb).unwrap();
    let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
        notedeck::NoteAction::Profile(*pk),
        ctx.ndb,
        cols,
        0,
        &mut columns.timeline_cache,
        &mut columns.threads,
        ctx.note_cache,
        ctx.pool,
        &txn,
        ctx.unknown_ids,
        ctx.accounts,
        ctx.global_wallet,
        ctx.zaps,
        ctx.img_cache,
        &mut columns.view_state,
        ui,
    );

    if let Some(action) = m_action {
        let col = cols.selected_mut();

        action.process(&mut col.router, &mut col.sheet_router);
    }
}

fn pfp_button(ctx: &mut AppContext, ui: &mut egui::Ui) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
    let helper = AnimationHelper::new(ui, "pfp-button", egui::vec2(max_size, max_size));

    let min_pfp_size = ICON_WIDTH;
    let cur_pfp_size = helper.scale_1d_pos(min_pfp_size);

    let txn = Transaction::new(ctx.ndb).expect("should be able to create txn");
    let profile_url = get_account_url(&txn, ctx.ndb, ctx.accounts.get_selected_account());

    let mut widget = ProfilePic::new(ctx.img_cache, profile_url).size(cur_pfp_size);

    ui.put(helper.get_animation_rect(), &mut widget);

    helper.take_animation_response()
}

/// The section of the chrome sidebar that starts at the
/// bottom and goes up
fn bottomup_sidebar(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
    options: SidebarOptions,
) -> Option<ChromePanelAction> {
    ui.add_space(8.0);

    let pfp_resp = pfp_button(ctx, ui).on_hover_cursor(egui::CursorIcon::PointingHand);

    // we skip this whole function in compact mode
    if options.contains(SidebarOptions::Compact) {
        return if pfp_resp.clicked() {
            Some(ChromePanelAction::Profile(
                ctx.accounts.get_selected_account().key.pubkey,
            ))
        } else {
            None
        };
    }

    let accounts_resp = accounts_button(ui).on_hover_cursor(egui::CursorIcon::PointingHand);
    let settings_resp = settings_button(ui).on_hover_cursor(egui::CursorIcon::PointingHand);

    let theme_action = match ui.ctx().theme() {
        egui::Theme::Dark => {
            let resp = ui
                .add(Button::new("☀").frame(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tr!(
                    ctx.i18n,
                    "Switch to light mode",
                    "Hover text for light mode toggle button"
                ));
            if resp.clicked() {
                Some(ChromePanelAction::SaveTheme(ThemePreference::Light))
            } else {
                None
            }
        }
        egui::Theme::Light => {
            let resp = ui
                .add(Button::new("🌙").frame(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tr!(
                    ctx.i18n,
                    "Switch to dark mode",
                    "Hover text for dark mode toggle button"
                ));
            if resp.clicked() {
                Some(ChromePanelAction::SaveTheme(ThemePreference::Dark))
            } else {
                None
            }
        }
    };

    let support_resp = support_button(ui).on_hover_cursor(egui::CursorIcon::PointingHand);

    let wallet_resp = ui
        .add(wallet_button())
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if ctx.args.options.contains(NotedeckOptions::Debug) {
        let r = ui
            .weak(format!("{}", ctx.frame_history.fps() as i32))
            .union(ui.weak(format!(
                "{:10.1}",
                ctx.frame_history.mean_frame_time() * 1e3
            )))
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        if r.clicked() {
            chrome.options.toggle(ChromeOptions::RepaintDebug);
        }

        if chrome.options.contains(ChromeOptions::RepaintDebug) {
            for cause in ui.ctx().repaint_causes() {
                chrome
                    .repaint_causes
                    .entry(cause)
                    .and_modify(|rc| {
                        *rc += 1;
                    })
                    .or_insert(1);
            }
            repaint_causes_window(ui, &chrome.repaint_causes)
        }

        #[cfg(feature = "memory")]
        {
            let mem_use = re_memory::MemoryUse::capture();
            if let Some(counted) = mem_use.counted {
                if ui
                    .label(format!("{}", format_bytes(counted as f64)))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    chrome.options.toggle(ChromeOptions::MemoryDebug);
                }
            }
            if let Some(resident) = mem_use.resident {
                ui.weak(format!("{}", format_bytes(resident as f64)));
            }

            if chrome.options.contains(ChromeOptions::MemoryDebug) {
                egui::Window::new("Memory Debug").show(ui.ctx(), memory_debug_ui);
            }
        }
    }

    if pfp_resp.clicked() {
        let pk = ctx.accounts.get_selected_account().key.pubkey;
        Some(ChromePanelAction::Profile(pk))
    } else if accounts_resp.clicked() {
        Some(ChromePanelAction::Account)
    } else if settings_resp.clicked() {
        Some(ChromePanelAction::Settings)
    } else if theme_action.is_some() {
        theme_action
    } else if support_resp.clicked() {
        Some(ChromePanelAction::Support)
    } else if wallet_resp.clicked() {
        Some(ChromePanelAction::Wallet)
    } else {
        None
    }
}

#[cfg(feature = "memory")]
fn memory_debug_ui(ui: &mut egui::Ui) {
    let Some(stats) = &re_memory::accounting_allocator::tracking_stats() else {
        ui.label("re_memory::accounting_allocator::set_tracking_callstacks(true); not set!!");
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(format!(
            "track_size_threshold {}",
            stats.track_size_threshold
        ));
        ui.label(format!(
            "untracked {} {}",
            stats.untracked.count,
            format_bytes(stats.untracked.size as f64)
        ));
        ui.label(format!(
            "stochastically_tracked {} {}",
            stats.stochastically_tracked.count,
            format_bytes(stats.stochastically_tracked.size as f64),
        ));
        ui.label(format!(
            "fully_tracked {} {}",
            stats.fully_tracked.count,
            format_bytes(stats.fully_tracked.size as f64)
        ));
        ui.label(format!(
            "overhead {} {}",
            stats.overhead.count,
            format_bytes(stats.overhead.size as f64)
        ));

        ui.separator();

        for (i, callstack) in stats.top_callstacks.iter().enumerate() {
            let full_bt = format!("{}", callstack.readable_backtrace);
            let mut lines = full_bt.lines().skip(5);
            let bt_header = lines.nth(0).map_or("??", |v| v);
            let header = format!(
                "#{} {bt_header} {}x {}",
                i + 1,
                callstack.extant.count,
                format_bytes(callstack.extant.size as f64)
            );

            egui::CollapsingHeader::new(header)
                .id_salt(("mem_cs", i))
                .show(ui, |ui| {
                    ui.label(lines.collect::<Vec<_>>().join("\n"));
                });
        }
    });
}

/// Pretty format a number of bytes by using SI notation (base2), e.g.
///
/// ```
/// # use re_format::format_bytes;
/// assert_eq!(format_bytes(123.0), "123 B");
/// assert_eq!(format_bytes(12_345.0), "12.1 KiB");
/// assert_eq!(format_bytes(1_234_567.0), "1.2 MiB");
/// assert_eq!(format_bytes(123_456_789.0), "118 MiB");
/// ```
#[cfg(feature = "memory")]
pub fn format_bytes(number_of_bytes: f64) -> String {
    /// The minus character: <https://www.compart.com/en/unicode/U+2212>
    /// Looks slightly different from the normal hyphen `-`.
    const MINUS: char = '−';

    if number_of_bytes < 0.0 {
        format!("{MINUS}{}", format_bytes(-number_of_bytes))
    } else if number_of_bytes == 0.0 {
        "0 B".to_owned()
    } else if number_of_bytes < 1.0 {
        format!("{number_of_bytes} B")
    } else if number_of_bytes < 20.0 {
        let is_integer = number_of_bytes.round() == number_of_bytes;
        if is_integer {
            format!("{number_of_bytes:.0} B")
        } else {
            format!("{number_of_bytes:.1} B")
        }
    } else if number_of_bytes < 10.0_f64.exp2() {
        format!("{number_of_bytes:.0} B")
    } else if number_of_bytes < 20.0_f64.exp2() {
        let decimals = (10.0 * number_of_bytes < 20.0_f64.exp2()) as usize;
        format!("{:.*} KiB", decimals, number_of_bytes / 10.0_f64.exp2())
    } else if number_of_bytes < 30.0_f64.exp2() {
        let decimals = (10.0 * number_of_bytes < 30.0_f64.exp2()) as usize;
        format!("{:.*} MiB", decimals, number_of_bytes / 20.0_f64.exp2())
    } else {
        let decimals = (10.0 * number_of_bytes < 40.0_f64.exp2()) as usize;
        format!("{:.*} GiB", decimals, number_of_bytes / 30.0_f64.exp2())
    }
}

fn repaint_causes_window(ui: &mut egui::Ui, causes: &HashMap<egui::RepaintCause, u64>) {
    egui::Window::new("Repaint Causes").show(ui.ctx(), |ui| {
        use egui_extras::{Column, TableBuilder};
        TableBuilder::new(ui)
            .column(Column::auto().at_least(600.0).resizable(true))
            .column(Column::auto().at_least(50.0).resizable(true))
            .column(Column::auto().at_least(50.0).resizable(true))
            .column(Column::remainder())
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.heading("file");
                });
                header.col(|ui| {
                    ui.heading("line");
                });
                header.col(|ui| {
                    ui.heading("count");
                });
                header.col(|ui| {
                    ui.heading("reason");
                });
            })
            .body(|mut body| {
                for (cause, hits) in causes.iter() {
                    body.row(30.0, |mut row| {
                        row.col(|ui| {
                            ui.label(cause.file.to_string());
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", cause.line));
                        });
                        row.col(|ui| {
                            ui.label(format!("{hits}"));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", &cause.reason));
                        });
                    });
                }
            });
    });
}

fn virtual_keyboard_ui(ui: &mut egui::Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(200));

    ui.put(rect, |ui: &mut egui::Ui| {
        ui.centered_and_justified(|ui| {
            ui.label("This is a keyboard");
        })
        .response
    });
}

struct SoftKeyboardAnim {
    skb_rect: Option<Rect>,
    anim_height: f32,
}

#[derive(Copy, Default, Clone, Eq, PartialEq, Debug)]
enum AnimState {
    /// It finished opening
    Opened,

    /// We started to open
    StartOpen,

    /// We started to close
    StartClose,

    /// We finished openning
    FinishedOpen,

    /// We finished to close
    FinishedClose,

    /// It finished closing
    #[default]
    Closed,

    /// We are animating towards open
    Opening,

    /// We are animating towards close
    Closing,
}

impl SoftKeyboardAnim {
    /// Advance the FSM based on current (anim_height) vs target (skb_rect.height()).
    /// Start*/Finished* are one-tick edge states used for signaling.
    fn changed(&self, state: AnimState) -> AnimState {
        const EPS: f32 = 0.01;

        let target = self.skb_rect.map_or(0.0, |r| r.height());
        let current = self.anim_height;

        let done = (current - target).abs() <= EPS;
        let going_up = target > current + EPS;
        let going_down = current > target + EPS;
        let target_is_closed = target <= EPS;

        match state {
            // Resting states: emit a Start* edge only when a move is requested,
            // and pick direction by the sign of (target - current).
            AnimState::Opened => {
                if done {
                    AnimState::Opened
                } else if going_up {
                    AnimState::StartOpen
                } else {
                    AnimState::StartClose
                }
            }
            AnimState::Closed => {
                if done {
                    AnimState::Closed
                } else if going_up {
                    AnimState::StartOpen
                } else {
                    AnimState::StartClose
                }
            }

            // Edge → flow
            AnimState::StartOpen => AnimState::Opening,
            AnimState::StartClose => AnimState::Closing,

            // Flow states: finish when we hit the target; if the target jumps across,
            // emit the opposite Start* to signal a reversal.
            AnimState::Opening => {
                if done {
                    if target_is_closed {
                        AnimState::FinishedClose
                    } else {
                        AnimState::FinishedOpen
                    }
                } else if going_down {
                    // target moved below current mid-flight → reversal
                    AnimState::StartClose
                } else {
                    AnimState::Opening
                }
            }
            AnimState::Closing => {
                if done {
                    if target_is_closed {
                        AnimState::FinishedClose
                    } else {
                        AnimState::FinishedOpen
                    }
                } else if going_up {
                    // target moved above current mid-flight → reversal
                    AnimState::StartOpen
                } else {
                    AnimState::Closing
                }
            }

            // Finish edges collapse to the stable resting states on the next tick.
            AnimState::FinishedOpen => AnimState::Opened,
            AnimState::FinishedClose => AnimState::Closed,
        }
    }
}

/// How "open" the softkeyboard is. This is an animated value
fn soft_keyboard_anim(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    chrome_options: &mut ChromeOptions,
) -> SoftKeyboardAnim {
    let skb_ctx = if chrome_options.contains(ChromeOptions::VirtualKeyboard) {
        SoftKeyboardContext::Virtual
    } else {
        SoftKeyboardContext::Platform {
            ppp: ui.ctx().pixels_per_point(),
        }
    };

    // move screen up if virtual keyboard intersects with input_rect
    let screen_rect = ui.ctx().screen_rect();
    let mut skb_rect: Option<Rect> = None;

    let keyboard_height =
        if let Some(vkb_rect) = ctx.soft_keyboard_rect(screen_rect, skb_ctx.clone()) {
            skb_rect = Some(vkb_rect);
            vkb_rect.height()
        } else {
            0.0
        };

    let anim_height =
        ui.ctx()
            .animate_value_with_time(egui::Id::new("keyboard_anim"), keyboard_height, 0.1);

    SoftKeyboardAnim {
        anim_height,
        skb_rect,
    }
}

fn try_toggle_virtual_keyboard(
    ctx: &egui::Context,
    options: NotedeckOptions,
    chrome_options: &mut ChromeOptions,
) {
    // handle virtual keyboard toggle here because why not
    if options.contains(NotedeckOptions::Debug) && ctx.input(|i| i.key_pressed(egui::Key::F1)) {
        chrome_options.toggle(ChromeOptions::VirtualKeyboard);
    }
}

/// All the logic which handles our keyboard visibility
fn keyboard_visibility(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    options: &mut ChromeOptions,
    soft_kb_anim_state: &mut AnimState,
) -> SoftKeyboardAnim {
    try_toggle_virtual_keyboard(ui.ctx(), ctx.args.options, options);

    let soft_kb_anim = soft_keyboard_anim(ui, ctx, options);

    let prev_state = *soft_kb_anim_state;
    let current_state = soft_kb_anim.changed(prev_state);
    *soft_kb_anim_state = current_state;

    if prev_state != current_state {
        tracing::debug!("soft kb state {prev_state:?} -> {current_state:?}");
    }

    match current_state {
        // we finished
        AnimState::FinishedOpen => {}

        // on first open, we setup our scroll target
        AnimState::StartOpen => {
            // when we first open the keyboard, check to see if the target soft
            // keyboard rect (the height at full open) intersects with any
            // input response rects from last frame
            //
            // If we do, then we set a bit that we need keyboard visibility.
            // We will use this bit to resize the screen based on the soft
            // keyboard animation state
            if let Some(skb_rect) = soft_kb_anim.skb_rect {
                if let Some(input_rect) = notedeck_ui::input_rect(ui) {
                    options.set(
                        ChromeOptions::KeyboardVisibility,
                        input_rect.intersects(skb_rect),
                    )
                }
            }
        }

        AnimState::FinishedClose => {
            // clear last input box position state
            notedeck_ui::clear_input_rect(ui);
        }

        AnimState::Closing => {}
        AnimState::Opened => {}
        AnimState::Closed => {}
        AnimState::Opening => {}
        AnimState::StartClose => {}
    };

    soft_kb_anim
}
