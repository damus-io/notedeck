// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use egui::{vec2, Button, Color32, Label, Layout, Rect, RichText, ThemePreference, Widget};
use egui_extras::{Size, StripBuilder};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::{
    tr, App, AppAction, AppContext, Localization, NotedeckOptions, NotedeckTextStyle, UserAccount,
    WalletType,
};
use notedeck_columns::{
    column::SelectionResult, timeline::kind::ListKind, timeline::TimelineKind, Damus,
};
use notedeck_dave::{Dave, DaveAvatar};
use notedeck_notebook::Notebook;
use notedeck_ui::{app_images, AnimationHelper, ProfilePic};

static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

pub struct Chrome {
    active: i32,
    open: bool,
    tab_selected: i32,
    apps: Vec<NotedeckApp>,

    #[cfg(feature = "memory")]
    show_memory_debug: bool,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            active: 0,
            tab_selected: 0,
            // sidemenu is not open by default on mobile/narrow uis
            open: !notedeck::ui::is_compiled_as_mobile(),
            apps: vec![],

            #[cfg(feature = "memory")]
            show_memory_debug: false,
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
    Profile(notedeck::enostr::Pubkey),
}

impl ChromePanelAction {
    fn columns_switch(ctx: &mut AppContext, chrome: &mut Chrome, kind: &TimelineKind) {
        chrome.switch_to_columns();

        let Some(columns_app) = chrome.get_columns_app() else {
            return;
        };

        if let Some(active_columns) = columns_app
            .decks_cache
            .active_columns_mut(ctx.i18n, ctx.accounts)
        {
            match active_columns.select_by_kind(kind) {
                SelectionResult::NewSelection(_index) => {
                    // great! no need to go to top yet
                }

                SelectionResult::AlreadySelected(_n) => {
                    // we already selected this, so scroll to top
                    columns_app.scroll_to_top();
                }

                SelectionResult::Failed => {
                    // oh no, something went wrong
                    // TODO(jb55): handle tab selection failure
                }
            }
        }
    }

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

    fn process(&self, ctx: &mut AppContext, chrome: &mut Chrome, ui: &mut egui::Ui) {
        match self {
            Self::SaveTheme(theme) => {
                ui.ctx().set_theme(*theme);
                ctx.settings.set_theme(*theme);
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

    fn get_columns_app(&mut self) -> Option<&mut Damus> {
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

    fn get_notebook(&mut self) -> Option<&mut Notebook> {
        for app in &mut self.apps {
            if let NotedeckApp::Notebook(notebook) = app {
                return Some(notebook);
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

    fn switch_to_notebook(&mut self) {
        for (i, app) in self.apps.iter().enumerate() {
            if let NotedeckApp::Notebook(_) = app {
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
            .horizontal(|mut hstrip| {
                hstrip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    if !ui.visuals().dark_mode {
                        let rect = ui.available_rect_before_wrap();
                        ui.painter().rect(
                            rect,
                            0,
                            notedeck_ui::colors::ALMOST_WHITE,
                            egui::Stroke::new(0.0, Color32::TRANSPARENT),
                            egui::StrokeKind::Inside,
                        );
                    }

                    StripBuilder::new(ui)
                        .size(Size::remainder())
                        .size(Size::remainder())
                        .vertical(|mut vstrip| {
                            vstrip.cell(|ui| {
                                _ = ui.vertical_centered(|ui| {
                                    self.topdown_sidebar(ui, app_ctx.i18n);
                                })
                            });
                            vstrip.cell(|ui| {
                                ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                                    if let Some(action) = bottomup_sidebar(self, app_ctx, ui) {
                                        got_action = Some(action);
                                    }
                                });
                            });
                        });

                    // vertical sidebar line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                hstrip.cell(|ui| {
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
        let side_panel_width: f32 = 74.0;
        ui.ctx().animate_bool(open_id, self.open) * side_panel_width
    }

    fn toolbar_height() -> f32 {
        48.0
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

        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );

        if !ui.visuals().dark_mode {
            ui.painter().rect(
                rect,
                0,
                notedeck_ui::colors::ALMOST_WHITE,
                egui::Stroke::new(0.0, Color32::TRANSPARENT),
                egui::StrokeKind::Inside,
            );
        }

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

                let btn_size: f32 = 20.0;
                if index == 0 {
                    if home_button(ui, btn_size).clicked() {
                        action = Some(ToolbarAction::Home);
                    }
                } else if index == 1 {
                    if let Some(dave) = self.get_dave() {
                        let rect = dave_toolbar_rect(ui, btn_size * 2.0);
                        if dave_button(dave.avatar_mut(), ui, rect).clicked() {
                            action = Some(ToolbarAction::Dave);
                        }
                    }
                } else if index == 2 && notifications_button(ui, btn_size).clicked() {
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
            //self.active = (self.active + 1) % (self.apps.len() as i32);
            self.open = !self.open;
        }

        ui.add_space(4.0);
        ui.add(milestone_name(i18n));
        ui.add_space(16.0);
        //let dark_mode = ui.ctx().style().visuals.dark_mode;
        if columns_button(ui)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.active = 0;
        }
        ui.add_space(32.0);

        if let Some(dave) = self.get_dave() {
            let rect = dave_sidebar_rect(ui);
            let dave_resp = dave_button(dave.avatar_mut(), ui, rect)
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if dave_resp.clicked() {
                self.switch_to_dave();
            }
        }
        //ui.add_space(32.0);

        if let Some(_notebook) = self.get_notebook() {
            if notebook_button(ui)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.switch_to_notebook();
            }
        }
    }
}

fn unseen_notification(
    columns: Option<&mut Damus>,
    ndb: &nostrdb::Ndb,
    current_pk: notedeck::enostr::Pubkey,
) -> bool {
    let Some(columns) = columns else {
        return false;
    };

    let Some(tl) = columns
        .timeline_cache
        .get_mut(&TimelineKind::Notifications(current_pk))
    else {
        return false;
    };

    let freshness = &mut tl.current_view_mut().freshness;
    freshness.update(|timestamp_last_viewed| {
        let filter = notedeck_columns::timeline::kind::notifications_filter(&current_pk)
            .since_mut(timestamp_last_viewed);
        let txn = Transaction::new(ndb).expect("txn");

        let Some(res) = ndb.query(&txn, &[filter], 1).ok() else {
            return false;
        };

        !res.is_empty()
    });

    freshness.has_unseen()
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

fn notifications_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    expanding_button(
        "notifications-button",
        size,
        app_images::notifications_light_image(),
        app_images::notifications_dark_image(),
        ui,
    )
}

fn home_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    expanding_button(
        "home-button",
        size,
        app_images::home_light_image(),
        app_images::home_dark_image(),
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

fn accounts_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "accounts-button",
        24.0,
        app_images::accounts_image().tint(ui.visuals().text_color()),
        app_images::accounts_image(),
        ui,
    )
}

fn notebook_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "notebook-button",
        40.0,
        app_images::algo_image(),
        app_images::algo_image(),
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

fn dave_toolbar_rect(ui: &mut egui::Ui, size: f32) -> Rect {
    let size = vec2(size, size);
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

    // let selected = ctx.accounts.cache.selected();

    // pfp_resp.context_menu(|ui| {
    //     for (pk, account) in &ctx.accounts.cache {
    //         let profile = ctx.ndb.get_profile_by_pubkey(&txn, pk).ok();
    //         let is_selected = *pk == selected.key.pubkey;
    //         let has_nsec = account.key.secret_key.is_some();

    //         let profile_peview_view = {
    //             let max_size = egui::vec2(ui.available_width(), 77.0);
    //             let resp = ui.allocate_response(max_size, egui::Sense::click());
    //             ui.allocate_new_ui(UiBuilder::new().max_rect(resp.rect), |ui| {
    //                 ui.add(
    //                     &mut ProfilePic::new(ctx.img_cache, get_profile_url(profile.as_ref()))
    //                         .size(24.0),
    //                 )
    //             })
    //         };

    //         // if let Some(op) = profile_peview_view {
    //         //     return_op = Some(match op {
    //         //         ProfilePreviewAction::SwitchTo => AccountsViewResponse::SelectAccount(*pk),
    //         //         ProfilePreviewAction::RemoveAccount => AccountsViewResponse::RemoveAccount(*pk),
    //         //     });
    //         // }
    //     }
    //     // if ui.menu_image_button(image, add_contents).clicked() {
    //     //     // ui.ctx().copy_text(url.to_owned());
    //     //     ui.close_menu();
    //     // }
    // });
}

/// The section of the chrome sidebar that starts at the
/// bottom and goes up
fn bottomup_sidebar(
    _chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) -> Option<ChromePanelAction> {
    ui.add_space(8.0);

    let pfp_resp = pfp_button(ctx, ui).on_hover_cursor(egui::CursorIcon::PointingHand);
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
        ui.weak(format!("{}", ctx.frame_history.fps() as i32));
        ui.weak(format!(
            "{:10.1}",
            ctx.frame_history.mean_frame_time() * 1e3
        ));

        #[cfg(feature = "memory")]
        {
            let mem_use = re_memory::MemoryUse::capture();
            if let Some(counted) = mem_use.counted {
                if ui
                    .label(format!("{}", format_bytes(counted as f64)))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    _chrome.show_memory_debug = !_chrome.show_memory_debug;
                }
            }
            if let Some(resident) = mem_use.resident {
                ui.weak(format!("{}", format_bytes(resident as f64)));
            }

            if _chrome.show_memory_debug {
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
