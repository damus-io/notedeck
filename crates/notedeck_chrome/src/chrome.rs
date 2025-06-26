// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use egui::{vec2, Button, Label, Layout, Rect, RichText, ThemePreference, Widget};
use egui_extras::{Size, StripBuilder};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::{App, AppAction, AppContext, NotedeckTextStyle, UserAccount, WalletType};
use notedeck_columns::{timeline::kind::ListKind, timeline::TimelineKind, Damus};

use notedeck_dave::{Dave, DaveAvatar};
use notedeck_ui::{app_images, AnimationHelper, ProfilePic};

static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

pub struct Chrome {
    active: i32,
    open: bool,
    tab_selected: i32,
    apps: Vec<NotedeckApp>,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            active: 0,
            tab_selected: 0,
            open: true,
            apps: vec![],
        }
    }
}

/// When you click the toolbar button, these actions
/// are returned
#[derive(Debug, Eq, PartialEq)]
pub enum ToolbarAction {
    Notifications,
    Dave,
    Home,
}

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    Wallet,
    Toolbar(ToolbarAction),
    SaveTheme(ThemePreference),
}

impl ChromePanelAction {
    fn columns_switch(ctx: &AppContext, chrome: &mut Chrome, kind: &TimelineKind) {
        chrome.switch_to_columns();

        if let Some(active_columns) = chrome
            .get_columns()
            .and_then(|cols| cols.decks_cache.active_columns_mut(ctx.accounts))
        {
            active_columns.select_by_kind(kind)
        }
    }

    fn columns_navigate(ctx: &AppContext, chrome: &mut Chrome, route: notedeck_columns::Route) {
        chrome.switch_to_columns();

        if let Some(c) = chrome
            .get_columns()
            .and_then(|columns| columns.decks_cache.selected_column_mut(ctx.accounts))
        {
            if c.router().routes().iter().any(|r| r == &route) {
                // return if we are already routing to accounts
                c.router_mut().go_back();
            } else {
                c.router_mut().route_to(route);
                //c..route_to(Route::relays());
            }
        };
    }

    fn process(&self, ctx: &AppContext, chrome: &mut Chrome, ui: &mut egui::Ui) {
        match self {
            Self::SaveTheme(theme) => {
                ui.ctx().options_mut(|o| {
                    o.theme_preference = *theme;
                });
                ctx.theme.save(*theme);
            }

            Self::Toolbar(toolbar_action) => match toolbar_action {
                ToolbarAction::Dave => chrome.switch_to_dave(),

                ToolbarAction::Home => {
                    Self::columns_switch(
                        ctx,
                        chrome,
                        &TimelineKind::List(ListKind::Contact(
                            ctx.accounts.get_selected_account().key.pubkey,
                        )),
                    );
                }

                ToolbarAction::Notifications => {
                    Self::columns_switch(
                        ctx,
                        chrome,
                        &TimelineKind::Notifications(
                            ctx.accounts.get_selected_account().key.pubkey,
                        ),
                    );
                }
            },

            Self::Support => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Support);
            }

            Self::Account => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::accounts());
            }

            Self::Settings => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Relays);
            }

            Self::Wallet => {
                Self::columns_navigate(
                    ctx,
                    chrome,
                    notedeck_columns::Route::Wallet(WalletType::Auto),
                );
            }
        }
    }
}

impl Chrome {
    pub fn new() -> Self {
        Chrome::default()
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn add_app(&mut self, app: NotedeckApp) {
        self.apps.push(app);
    }

    fn get_columns(&mut self) -> Option<&mut Damus> {
        for app in &mut self.apps {
            if let NotedeckApp::Columns(cols) = app {
                return Some(cols);
            }
        }

        None
    }

    fn get_dave(&mut self) -> Option<&mut Dave> {
        for app in &mut self.apps {
            if let NotedeckApp::Dave(dave) = app {
                return Some(dave);
            }
        }

        None
    }

    fn switch_to_dave(&mut self) {
        for (i, app) in self.apps.iter().enumerate() {
            if let NotedeckApp::Dave(_) = app {
                self.active = i as i32;
            }
        }
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
    fn panel(
        &mut self,
        app_ctx: &mut AppContext,
        builder: StripBuilder,
        amt_open: f32,
    ) -> Option<ChromePanelAction> {
        let mut got_action: Option<ChromePanelAction> = None;

        builder
            .size(Size::exact(amt_open)) // collapsible sidebar
            .size(Size::remainder()) // the main app contents
            .clip(true)
            .horizontal(|mut strip| {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    if !ui.visuals().dark_mode {
                        let rect = ui.available_rect_before_wrap();
                        ui.painter().rect(
                            rect,
                            0,
                            notedeck_ui::colors::ALMOST_WHITE,
                            egui::Stroke::new(0.0, egui::Color32::TRANSPARENT),
                            egui::StrokeKind::Inside,
                        );
                    }

                    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        self.topdown_sidebar(ui);
                    });

                    ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                        if let Some(action) = bottomup_sidebar(app_ctx, ui) {
                            got_action = Some(action);
                        }
                    });

                    // vertical sidebar line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                strip.cell(|ui| {
                    /*
                    let rect = ui.available_rect_before_wrap();
                    ui.painter().rect(
                        rect,
                        0,
                        egui::Color32::RED,
                        egui::Stroke::new(1.0, egui::Color32::BLUE),
                        egui::StrokeKind::Inside,
                    );
                    */

                    if let Some(action) = self.apps[self.active as usize].update(app_ctx, ui) {
                        chrome_handle_app_action(self, app_ctx, action, ui);
                    }
                });
            });

        got_action
    }

    /// How far is the chrome panel expanded?
    fn amount_open(&self, ui: &mut egui::Ui) -> f32 {
        let open_id = egui::Id::new("chrome_open");
        let side_panel_width: f32 = 70.0;
        ui.ctx().animate_bool(open_id, self.open) * side_panel_width
    }

    fn toolbar_height() -> f32 {
        60.0
    }

    /// On narrow layouts, we have a toolbar
    fn toolbar_chrome(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> Option<ChromePanelAction> {
        let mut got_action: Option<ChromePanelAction> = None;
        let amt_open = self.amount_open(ui);

        StripBuilder::new(ui)
            .size(Size::remainder()) // top cell
            .size(Size::exact(Self::toolbar_height())) // bottom cell
            .vertical(|mut strip| {
                strip.strip(|builder| {
                    // the chrome panel is nested above the toolbar

                    got_action = self.panel(ctx, builder, amt_open);
                });

                strip.cell(|ui| {
                    if let Some(action) = self.toolbar(ui) {
                        got_action = Some(ChromePanelAction::Toolbar(action))
                    }
                });
            });

        got_action
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<ToolbarAction> {
        use egui_tabs::{TabColor, Tabs};

        let rs = Tabs::new(3)
            .selected(self.tab_selected)
            .hover_bg(TabColor::none())
            .selected_fg(TabColor::none())
            .selected_bg(TabColor::none())
            .height(Self::toolbar_height())
            .layout(Layout::centered_and_justified(egui::Direction::TopDown))
            .show(ui, |ui, state| {
                let index = state.index();

                let mut action: Option<ToolbarAction> = None;

                if index == 0 {
                    if home_button(ui).clicked() {
                        action = Some(ToolbarAction::Home);
                    }
                } else if index == 1 {
                    if let Some(dave) = self.get_dave() {
                        let rect = dave_toolbar_rect(ui);
                        if dave_button(dave.avatar_mut(), ui, rect).clicked() {
                            action = Some(ToolbarAction::Dave);
                        }
                    }
                } else if index == 2 && notifications_button(ui).clicked() {
                    action = Some(ToolbarAction::Notifications);
                }

                action
            })
            .inner();

        for maybe_r in rs {
            if maybe_r.inner.is_some() {
                return maybe_r.inner;
            }
        }

        None
    }

    /// Show the side menu or bar, depending on if we're on a narrow
    /// or wide screen.
    ///
    /// The side menu should hover over the screen, while the side bar
    /// is collapsible but persistent on the screen.
    fn show(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<ChromePanelAction> {
        ui.spacing_mut().item_spacing.x = 0.0;

        if notedeck::ui::is_narrow(ui.ctx()) {
            self.toolbar_chrome(ctx, ui)
        } else {
            let amt_open = self.amount_open(ui);
            self.panel(ctx, StripBuilder::new(ui), amt_open)
        }
    }

    fn topdown_sidebar(&mut self, ui: &mut egui::Ui) {
        // macos needs a bit of space to make room for window
        // minimize/close buttons
        if cfg!(target_os = "macos") {
            ui.add_space(28.0);
        } else {
            // we still want *some* padding so that it aligns with the + button regardless
            ui.add_space(notedeck_ui::constants::FRAME_MARGIN.into());
        }

        if ui.add(expand_side_panel_button()).clicked() {
            //self.active = (self.active + 1) % (self.apps.len() as i32);
            self.open = !self.open;
        }

        ui.add_space(4.0);
        ui.add(milestone_name());
        ui.add_space(16.0);
        //let dark_mode = ui.ctx().style().visuals.dark_mode;
        {
            let col_resp = columns_button(ui);
            if col_resp.clicked() {
                self.active = 0;
            } else if col_resp.hovered() {
                notedeck_ui::show_pointer(ui);
            }
        }
        ui.add_space(32.0);

        if let Some(dave) = self.get_dave() {
            let rect = dave_sidebar_rect(ui);
            let dave_resp = dave_button(dave.avatar_mut(), ui, rect);
            if dave_resp.clicked() {
                self.switch_to_dave();
            } else if dave_resp.hovered() {
                notedeck_ui::show_pointer(ui);
            }
        }
    }
}

impl notedeck::App for Chrome {
    fn update(&mut self, ctx: &mut notedeck::AppContext, ui: &mut egui::Ui) -> Option<AppAction> {
        if let Some(action) = self.show(ctx, ui) {
            action.process(ctx, self, ui);
        }
        // TODO: unify this constant with the columns side panel width. ui crate?
        None
    }
}

fn milestone_name() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        ui.vertical_centered(|ui| {
            let font = egui::FontId::new(
                notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Tiny),
                egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(
                Label::new(
                    RichText::new("BETA")
                        .color(ui.style().visuals.noninteractive().fg_stroke.color)
                        .font(font),
                )
                .selectable(false),
            )
            .on_hover_text(
                "Notedeck is a beta product. Expect bugs and contact us when you run into issues.",
            )
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

fn expanding_button(
    name: &'static str,
    img_size: f32,
    light_img: egui::Image,
    dark_img: egui::Image,
    ui: &mut egui::Ui,
) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
    let img = if ui.visuals().dark_mode {
        dark_img
    } else {
        light_img
    };

    let helper = AnimationHelper::new(ui, name, egui::vec2(max_size, max_size));

    let cur_img_size = helper.scale_1d_pos(img_size);
    img.paint_at(
        ui,
        helper
            .get_animation_rect()
            .shrink((max_size - cur_img_size) / 2.0),
    );

    helper.take_animation_response()
}

fn support_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "help-button",
        16.0,
        app_images::help_light_image(),
        app_images::help_dark_image(),
        ui,
    )
}

fn settings_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "settings-button",
        32.0,
        app_images::settings_light_image(),
        app_images::settings_dark_image(),
        ui,
    )
}

fn notifications_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "notifications-button",
        24.0,
        app_images::notifications_button_image(),
        app_images::notifications_button_image(),
        ui,
    )
}

fn home_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "home-button",
        24.0,
        app_images::home_button_image(),
        app_images::home_button_image(),
        ui,
    )
}

fn columns_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "columns-button",
        40.0,
        app_images::columns_image(),
        app_images::columns_image(),
        ui,
    )
}

fn dave_sidebar_rect(ui: &mut egui::Ui) -> Rect {
    let size = vec2(60.0, 60.0);
    let available = ui.available_rect_before_wrap();
    let center_x = available.center().x;
    let center_y = available.top();
    egui::Rect::from_center_size(egui::pos2(center_x, center_y), size)
}

fn dave_toolbar_rect(ui: &mut egui::Ui) -> Rect {
    let size = vec2(60.0, 60.0);
    let available = ui.available_rect_before_wrap();
    let center_x = available.center().x;
    let center_y = available.center().y;
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

        let mut img = app_images::wallet_image().max_width(img_size);

        if !ui.visuals().dark_mode {
            img = img.tint(egui::Color32::BLACK);
        }

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

        AppAction::Note(note_action) => {
            chrome.switch_to_columns();
            let Some(columns) = chrome.get_columns() else {
                return;
            };

            let txn = Transaction::new(ctx.ndb).unwrap();

            let cols = columns
                .decks_cache
                .active_columns_mut(ctx.accounts)
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
                ui,
            );

            if let Some(action) = m_action {
                let col = cols.column_mut(0);

                action.process(&mut col.router, &mut col.sheet_router);
            }
        }
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
fn bottomup_sidebar(ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<ChromePanelAction> {
    ui.add_space(8.0);

    let pfp_resp = pfp_button(ctx, ui);
    let settings_resp = settings_button(ui);

    let theme_action = match ui.ctx().theme() {
        egui::Theme::Dark => {
            let resp = ui
                .add(Button::new("☀").frame(false))
                .on_hover_text("Switch to light mode");
            if resp.hovered() {
                notedeck_ui::show_pointer(ui);
            }
            if resp.clicked() {
                Some(ChromePanelAction::SaveTheme(ThemePreference::Light))
            } else {
                None
            }
        }
        egui::Theme::Light => {
            let resp = ui
                .add(Button::new("🌙").frame(false))
                .on_hover_text("Switch to dark mode");
            if resp.hovered() {
                notedeck_ui::show_pointer(ui);
            }
            if resp.clicked() {
                Some(ChromePanelAction::SaveTheme(ThemePreference::Dark))
            } else {
                None
            }
        }
    };

    let support_resp = support_button(ui);

    let wallet_resp = ui.add(wallet_button());

    if ctx.args.debug {
        ui.weak(format!("{}", ctx.frame_history.fps() as i32));
        ui.weak(format!(
            "{:10.1}",
            ctx.frame_history.mean_frame_time() * 1e3
        ));
    }

    if pfp_resp.hovered()
        || settings_resp.hovered()
        || support_resp.hovered()
        || wallet_resp.hovered()
    {
        notedeck_ui::show_pointer(ui);
    }

    if pfp_resp.clicked() {
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
