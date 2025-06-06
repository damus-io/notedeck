// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use egui::{Button, Label, Layout, RichText, ThemePreference, Widget, vec2};
use egui_extras::{Size, StripBuilder};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::{
    App, AppAction, AppContext, NotedeckTextStyle, UserAccount, WalletType,
    profile::get_profile_url,
};
use notedeck_columns::Damus;
use notedeck_dave::{Dave, DaveAvatar};
use notedeck_ui::{AnimationHelper, ProfilePic};

static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

pub struct Chrome {
    active: i32,
    open: bool,
    apps: Vec<NotedeckApp>,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            active: 0,
            open: true,
            apps: vec![],
        }
    }
}

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    Wallet,
    SaveTheme(ThemePreference),
}

impl ChromePanelAction {
    fn columns_navigate(ctx: &AppContext, chrome: &mut Chrome, route: notedeck_columns::Route) {
        chrome.switch_to_columns();

        if let Some(c) = chrome
            .get_columns()
            .and_then(|columns| columns.decks_cache.first_column_mut(ctx.accounts))
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

    /// Show the side menu or bar, depending on if we're on a narrow
    /// or wide screen.
    ///
    /// The side menu should hover over the screen, while the side bar
    /// is collapsible but persistent on the screen.
    fn show(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<ChromePanelAction> {
        ui.spacing_mut().item_spacing.x = 0.0;

        let mut got_action: Option<ChromePanelAction> = None;
        let side_panel_width: f32 = 70.0;

        let open_id = egui::Id::new("chrome_open");
        let amt_open = ui.ctx().animate_bool(open_id, self.open) * side_panel_width;

        StripBuilder::new(ui)
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
                        if let Some(action) = bottomup_sidebar(ctx, ui) {
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

                    if let Some(action) = self.apps[self.active as usize].update(ctx, ui) {
                        chrome_handle_app_action(self, ctx, action, ui);
                    }
                });
            });

        got_action
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
            let dave_resp = dave_button(dave.avatar_mut(), ui);
            if dave_resp.clicked() {
                self.active = 1;
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
        let img_data = egui::include_image!("../../../assets/damus_rounded_80.png");
        let img = egui::Image::new(img_data)
            .max_width(img_size)
            .sense(egui::Sense::click());

        ui.add(img)
    }
}

fn expanding_button(
    name: &'static str,
    img_size: f32,
    light_img: &egui::ImageSource,
    dark_img: &egui::ImageSource,
    ui: &mut egui::Ui,
) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
    let img_data = if ui.visuals().dark_mode {
        dark_img
    } else {
        light_img
    };
    let img = egui::Image::new(img_data.clone()).max_width(img_size);

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
        &egui::include_image!("../../../assets/icons/help_icon_inverted_4x.png"),
        &egui::include_image!("../../../assets/icons/help_icon_dark_4x.png"),
        ui,
    )
}

fn settings_button(ui: &mut egui::Ui) -> egui::Response {
    expanding_button(
        "settings-button",
        32.0,
        &egui::include_image!("../../../assets/icons/settings_light_4x.png"),
        &egui::include_image!("../../../assets/icons/settings_dark_4x.png"),
        ui,
    )
}

fn columns_button(ui: &mut egui::Ui) -> egui::Response {
    let btn = egui::include_image!("../../../assets/icons/columns_80.png");
    expanding_button("columns-button", 40.0, &btn, &btn, ui)
}

fn dave_button(avatar: Option<&mut DaveAvatar>, ui: &mut egui::Ui) -> egui::Response {
    if let Some(avatar) = avatar {
        let size = vec2(60.0, 60.0);
        let available = ui.available_rect_before_wrap();
        let center_x = available.center().x;
        let rect = egui::Rect::from_center_size(egui::pos2(center_x, available.top()), size);
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
    account: Option<&UserAccount>,
) -> &'a str {
    if let Some(selected_account) = account {
        if let Ok(profile) = ndb.get_profile_by_pubkey(txn, selected_account.key.pubkey.bytes()) {
            get_profile_url_owned(Some(profile))
        } else {
            get_profile_url_owned(None)
        }
    } else {
        get_profile_url(None)
    }
}

fn wallet_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 24.0;

        let max_size = img_size * ICON_EXPANSION_MULTIPLE;
        let img_data = egui::include_image!("../../../assets/icons/wallet-icon.svg");

        let mut img = egui::Image::new(img_data).max_width(img_size);

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
