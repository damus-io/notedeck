use egui::{
    vec2, CursorIcon, InnerResponse, Label, Layout, Margin, RichText, ScrollArea, Separator,
    Stroke, Widget,
};
use tracing::{error, info};

use crate::{
    app::{get_active_columns_mut, get_decks_mut},
    app_style::DECK_ICON_SIZE,
    decks::{DecksAction, DecksCache},
    nav::SwitchingAction,
    route::Route,
};

use enostr::{RelayPool, RelayStatus};
use notedeck::{tr, Accounts, Localization, MediaJobSender, NotedeckTextStyle, UserAccount};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    app_images, colors, ProfilePic, View,
};

use super::configure_deck::deck_icon;

pub static SIDE_PANEL_WIDTH: f32 = 68.0;
static ICON_WIDTH: f32 = 40.0;

pub struct DesktopSidePanel<'a> {
    selected_account: &'a UserAccount,
    decks_cache: &'a DecksCache,
    i18n: &'a mut Localization,
    ndb: &'a nostrdb::Ndb,
    img_cache: &'a mut notedeck::Images,
    jobs: &'a MediaJobSender,
    current_route: Option<&'a Route>,
    pool: &'a RelayPool,
}

impl View for DesktopSidePanel<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SidePanelAction {
    Home,
    Columns,
    ComposeNote,
    Search,
    ExpandSidePanel,
    NewDeck,
    SwitchDeck(usize),
    EditDeck(usize),
    Wallet,
    Profile,
    Settings,
    Relays,
    Accounts,
    Support,
}

pub struct SidePanelResponse {
    pub response: egui::Response,
    pub action: SidePanelAction,
}

impl SidePanelResponse {
    fn new(action: SidePanelAction, response: egui::Response) -> Self {
        SidePanelResponse { action, response }
    }
}

impl<'a> DesktopSidePanel<'a> {
    pub fn new(
        selected_account: &'a UserAccount,
        decks_cache: &'a DecksCache,
        i18n: &'a mut Localization,
        ndb: &'a nostrdb::Ndb,
        img_cache: &'a mut notedeck::Images,
        jobs: &'a MediaJobSender,
        current_route: Option<&'a Route>,
        pool: &'a RelayPool,
    ) -> Self {
        Self {
            selected_account,
            decks_cache,
            i18n,
            ndb,
            img_cache,
            jobs,
            current_route,
            pool,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<SidePanelResponse> {
        let frame =
            egui::Frame::new().inner_margin(Margin::same(notedeck_ui::constants::FRAME_MARGIN));

        if !ui.visuals().dark_mode {
            let rect = ui.available_rect_before_wrap();
            ui.painter().rect(
                rect,
                0,
                colors::ALMOST_WHITE,
                egui::Stroke::new(0.0, egui::Color32::TRANSPARENT),
                egui::StrokeKind::Inside,
            );
        }

        frame.show(ui, |ui| self.show_inner(ui)).inner
    }

    fn show_inner(&mut self, ui: &mut egui::Ui) -> Option<SidePanelResponse> {
        let avatar_size = 40.0;
        let bottom_padding = 8.0;
        let connectivity_indicator_height = 48.0;
        let is_read_only = self.selected_account.key.secret_key.is_none();
        let read_only_label_height = if is_read_only { 16.0 } else { 0.0 };
        let avatar_section_height =
            avatar_size + bottom_padding + read_only_label_height + connectivity_indicator_height;

        ui.vertical(|ui| {
            #[cfg(target_os = "macos")]
            ui.add_space(32.0);

            let available_for_scroll = ui.available_height() - avatar_section_height;

            let scroll_out = ScrollArea::vertical()
                .max_height(available_for_scroll)
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        let home_resp = ui.add(home_button());
                        let compose_resp = ui
                            .add(crate::ui::post::compose_note_button(ui.visuals().dark_mode))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        let search_resp = ui.add(search_button(self.current_route));
                        let settings_resp = ui.add(settings_button(self.current_route));
                        let wallet_resp = ui.add(wallet_button(self.current_route));

                        let profile_resp = ui.add(profile_button(
                            self.current_route,
                            self.selected_account.key.pubkey,
                        ));

                        let support_resp = ui.add(support_button(self.current_route));

                        ui.add(Separator::default().horizontal().spacing(8.0).shrink(4.0));

                        ui.add_space(8.0);
                        ui.add(egui::Label::new(
                            RichText::new(tr!(
                                self.i18n,
                                "DECKS",
                                "Label for decks section in side panel"
                            ))
                            .size(11.0)
                            .color(ui.visuals().noninteractive().fg_stroke.color),
                        ));
                        ui.add_space(8.0);

                        let column_resp = ui.add(add_column_button());
                        let add_deck_resp = ui.add(add_deck_button(self.i18n));

                        let decks_inner = show_decks(ui, self.decks_cache, self.selected_account);

                        (
                            home_resp,
                            compose_resp,
                            search_resp,
                            column_resp,
                            settings_resp,
                            profile_resp,
                            wallet_resp,
                            support_resp,
                            add_deck_resp,
                            decks_inner,
                        )
                    })
                });

            let (
                home_resp,
                compose_resp,
                search_resp,
                column_resp,
                settings_resp,
                profile_resp,
                wallet_resp,
                support_resp,
                add_deck_resp,
                decks_inner,
            ) = scroll_out.inner.inner;

            let remaining = ui.available_height();
            if remaining > avatar_section_height {
                ui.add_space(remaining - avatar_section_height);
            }

            // Connectivity indicator
            let connectivity_resp = ui
                .with_layout(Layout::top_down(egui::Align::Center), |ui| {
                    connectivity_indicator(ui, self.pool, self.current_route)
                })
                .inner;

            let pfp_resp = ui
                .with_layout(Layout::top_down(egui::Align::Center), |ui| {
                    let is_read_only = self.selected_account.key.secret_key.is_none();

                    if is_read_only {
                        ui.add(
                            Label::new(
                                RichText::new(tr!(
                                    self.i18n,
                                    "Read only",
                                    "Label for read-only profile mode"
                                ))
                                .size(notedeck::fonts::get_font_size(
                                    ui.ctx(),
                                    &NotedeckTextStyle::Tiny,
                                ))
                                .color(ui.visuals().warn_fg_color),
                            )
                            .selectable(false),
                        );
                        ui.add_space(4.0);
                    }

                    let txn = nostrdb::Transaction::new(self.ndb).ok();
                    let profile_url = if let Some(ref txn) = txn {
                        if let Ok(profile) = self
                            .ndb
                            .get_profile_by_pubkey(txn, self.selected_account.key.pubkey.bytes())
                        {
                            notedeck::profile::get_profile_url(Some(&profile))
                        } else {
                            notedeck::profile::no_pfp_url()
                        }
                    } else {
                        notedeck::profile::no_pfp_url()
                    };

                    let resp = ui
                        .add(
                            &mut ProfilePic::new(self.img_cache, self.jobs, profile_url)
                                .size(avatar_size)
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);

                    // Draw border if Accounts route is active
                    let is_accounts_active = self
                        .current_route
                        .map_or(false, |r| matches!(r, Route::Accounts(_)));
                    if is_accounts_active {
                        let rect = resp.rect;
                        let radius = avatar_size / 2.0;
                        ui.painter().circle_stroke(
                            rect.center(),
                            radius + 2.0,
                            Stroke::new(1.5, ui.visuals().text_color()),
                        );
                    }

                    resp
                })
                .inner;

            if connectivity_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::Relays,
                    connectivity_resp,
                ))
            } else if home_resp.clicked() {
                Some(SidePanelResponse::new(SidePanelAction::Home, home_resp))
            } else if pfp_resp.clicked() {
                Some(SidePanelResponse::new(SidePanelAction::Accounts, pfp_resp))
            } else if compose_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::ComposeNote,
                    compose_resp,
                ))
            } else if search_resp.clicked() {
                Some(SidePanelResponse::new(SidePanelAction::Search, search_resp))
            } else if column_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::Columns,
                    column_resp,
                ))
            } else if settings_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::Settings,
                    settings_resp,
                ))
            } else if profile_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::Profile,
                    profile_resp,
                ))
            } else if wallet_resp.clicked() {
                Some(SidePanelResponse::new(SidePanelAction::Wallet, wallet_resp))
            } else if support_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::Support,
                    support_resp,
                ))
            } else if add_deck_resp.clicked() {
                Some(SidePanelResponse::new(
                    SidePanelAction::NewDeck,
                    add_deck_resp,
                ))
            } else if decks_inner.response.secondary_clicked() {
                info!("decks inner secondary click");
                if let Some(clicked_index) = decks_inner.inner {
                    Some(SidePanelResponse::new(
                        SidePanelAction::EditDeck(clicked_index),
                        decks_inner.response,
                    ))
                } else {
                    None
                }
            } else if decks_inner.response.clicked() {
                if let Some(clicked_index) = decks_inner.inner {
                    Some(SidePanelResponse::new(
                        SidePanelAction::SwitchDeck(clicked_index),
                        decks_inner.response,
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .inner
    }

    pub fn perform_action(
        decks_cache: &mut DecksCache,
        accounts: &Accounts,
        action: SidePanelAction,
        i18n: &mut Localization,
    ) -> Option<SwitchingAction> {
        let router = get_active_columns_mut(i18n, accounts, decks_cache).get_selected_router();
        let mut switching_response = None;
        match action {
            SidePanelAction::Home => {
                let pubkey = accounts.get_selected_account().key.pubkey;
                let home_route =
                    Route::timeline(crate::timeline::TimelineKind::contact_list(pubkey));

                if router.top() == &home_route {
                    // TODO: implement scroll to top when already on home route
                } else {
                    router.route_to(home_route);
                }
            }
            SidePanelAction::Columns => {
                if router
                    .routes()
                    .iter()
                    .any(|r| matches!(r, Route::AddColumn(_)))
                {
                    router.go_back();
                } else {
                    get_active_columns_mut(i18n, accounts, decks_cache).new_column_picker();
                }
            }
            SidePanelAction::ComposeNote => {
                let can_post = accounts.get_selected_account().key.secret_key.is_some();

                if !can_post {
                    router.route_to(Route::accounts());
                } else if router.routes().iter().any(|r| r == &Route::ComposeNote) {
                    router.go_back();
                } else {
                    router.route_to(Route::ComposeNote);
                }
            }
            SidePanelAction::Search => {
                if router.top() == &Route::Search {
                    router.go_back();
                } else {
                    router.route_to(Route::Search);
                }
            }
            SidePanelAction::ExpandSidePanel => {
                info!("Clicked expand side panel button");
            }
            SidePanelAction::NewDeck => {
                if router.routes().iter().any(|r| r == &Route::NewDeck) {
                    router.go_back();
                } else {
                    router.route_to(Route::NewDeck);
                }
            }
            SidePanelAction::SwitchDeck(index) => {
                switching_response = Some(crate::nav::SwitchingAction::Decks(DecksAction::Switch(
                    index,
                )))
            }
            SidePanelAction::EditDeck(index) => {
                if router.routes().iter().any(|r| r == &Route::EditDeck(index)) {
                    router.go_back();
                } else {
                    switching_response = Some(crate::nav::SwitchingAction::Decks(
                        DecksAction::Switch(index),
                    ));
                    if let Some(edit_deck) = get_decks_mut(i18n, accounts, decks_cache)
                        .decks_mut()
                        .get_mut(index)
                    {
                        edit_deck
                            .columns_mut()
                            .get_selected_router()
                            .route_to(Route::EditDeck(index));
                    } else {
                        error!("Cannot push EditDeck route to index {}", index);
                    }
                }
            }
            SidePanelAction::Wallet => 's: {
                if router
                    .routes()
                    .iter()
                    .any(|r| matches!(r, Route::Wallet(_)))
                {
                    router.go_back();
                    break 's;
                }

                router.route_to(Route::Wallet(notedeck::WalletType::Auto));
            }
            SidePanelAction::Profile => {
                let pubkey = accounts.get_selected_account().key.pubkey;
                if router.routes().iter().any(|r| r == &Route::profile(pubkey)) {
                    router.go_back();
                } else {
                    router.route_to(Route::profile(pubkey));
                }
            }
            SidePanelAction::Settings => {
                if router.routes().iter().any(|r| r == &Route::Settings) {
                    router.go_back();
                } else {
                    router.route_to(Route::Settings);
                }
            }
            SidePanelAction::Relays => {
                if router.routes().iter().any(|r| r == &Route::Relays) {
                    router.go_back();
                } else {
                    router.route_to(Route::relays());
                }
            }
            SidePanelAction::Accounts => {
                if router
                    .routes()
                    .iter()
                    .any(|r| matches!(r, Route::Accounts(_)))
                {
                    router.go_back();
                } else {
                    router.route_to(Route::accounts());
                }
            }
            SidePanelAction::Support => {
                if router.routes().iter().any(|r| r == &Route::Support) {
                    router.go_back();
                } else {
                    router.route_to(Route::Support);
                }
            }
        }
        switching_response
    }
}

fn add_column_button() -> impl Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let img = if ui.visuals().dark_mode {
            app_images::add_column_dark_image()
        } else {
            app_images::add_column_light_image()
        };

        let helper = AnimationHelper::new(ui, "add-column-button", vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Add new column")
    }
}

pub fn search_button_impl(color: egui::Color32, line_width: f32, is_active: bool) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let min_line_width_circle = line_width;
        let min_line_width_handle = line_width;
        let helper = AnimationHelper::new(ui, "search-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());

        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                notedeck_ui::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let cur_line_width_circle = helper.scale_1d_pos(min_line_width_circle);
        let cur_line_width_handle = helper.scale_1d_pos(min_line_width_handle);
        let min_outer_circle_radius = helper.scale_radius(15.0);
        let cur_outer_circle_radius = helper.scale_1d_pos(min_outer_circle_radius);
        let min_handle_length = 7.0;
        let cur_handle_length = helper.scale_1d_pos(min_handle_length);

        let circle_center = helper.scale_from_center(-2.0, -2.0);

        let handle_vec = vec2(
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        );

        let handle_pos_1 = circle_center + (handle_vec * (cur_outer_circle_radius - 3.0));
        let handle_pos_2 =
            circle_center + (handle_vec * (cur_outer_circle_radius + cur_handle_length));

        let icon_color = if is_active {
            ui.visuals().strong_text_color()
        } else {
            color
        };
        let circle_stroke = Stroke::new(cur_line_width_circle, icon_color);
        let handle_stroke = Stroke::new(cur_line_width_handle, icon_color);

        painter.line_segment([handle_pos_1, handle_pos_2], handle_stroke);
        painter.circle(
            circle_center,
            min_outer_circle_radius,
            ui.style().visuals.widgets.inactive.weak_bg_fill,
            circle_stroke,
        );

        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Open search")
    }
}

pub fn search_button(current_route: Option<&Route>) -> impl Widget + '_ {
    let is_active = matches!(current_route, Some(Route::Search));
    move |ui: &mut egui::Ui| {
        let icon_color = notedeck_ui::side_panel_icon_tint(ui);
        search_button_impl(icon_color, 1.5, is_active).ui(ui)
    }
}

// TODO: convert to responsive button when expanded side panel impl is finished

fn add_deck_button<'a>(i18n: &'a mut Localization) -> impl Widget + 'a {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 40.0;

        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img = app_images::new_deck_image().max_width(img_size);

        let helper = AnimationHelper::new(ui, "new-deck-icon", vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(tr!(
                i18n,
                "Add new deck",
                "Tooltip text for adding a new deck button"
            ))
    }
}

fn show_decks<'a>(
    ui: &mut egui::Ui,
    decks_cache: &'a DecksCache,
    selected_account: &'a UserAccount,
) -> InnerResponse<Option<usize>> {
    let show_decks_id = ui.id().with("show-decks");
    let account_id = selected_account.key.pubkey;
    let (cur_decks, account_id) = (
        decks_cache.decks(&account_id),
        show_decks_id.with(account_id),
    );
    let active_index = cur_decks.active_index();

    let (_, mut resp) = ui.allocate_exact_size(vec2(0.0, 0.0), egui::Sense::click());
    let mut clicked_index = None;
    for (index, deck) in cur_decks.decks().iter().enumerate() {
        let highlight = index == active_index;
        let deck_icon_resp = ui
            .add(deck_icon(
                account_id.with(index),
                Some(deck.icon),
                DECK_ICON_SIZE,
                40.0,
                highlight,
            ))
            .on_hover_text_at_pointer(&deck.name)
            .on_hover_cursor(CursorIcon::PointingHand);
        if deck_icon_resp.clicked() || deck_icon_resp.secondary_clicked() {
            clicked_index = Some(index);
        }
        resp = resp.union(deck_icon_resp);
    }
    InnerResponse::new(clicked_index, resp)
}

fn settings_button(current_route: Option<&Route>) -> impl Widget + '_ {
    let is_active = matches!(current_route, Some(Route::Settings));
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let helper = AnimationHelper::new(ui, "settings-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());
        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                notedeck_ui::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let img = if ui.visuals().dark_mode {
            app_images::settings_dark_image()
        } else {
            app_images::settings_light_image()
        };
        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );
        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Settings")
    }
}

fn profile_button(current_route: Option<&Route>, pubkey: enostr::Pubkey) -> impl Widget + '_ {
    let is_active = matches!(
        current_route,
        Some(Route::Timeline(crate::timeline::TimelineKind::Profile(pk))) if *pk == pubkey
    );
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let helper = AnimationHelper::new(ui, "profile-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());
        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                notedeck_ui::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let img = app_images::profile_image().tint(notedeck_ui::side_panel_icon_tint(ui));
        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );
        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Profile")
    }
}

fn wallet_button(current_route: Option<&Route>) -> impl Widget + '_ {
    let is_active = matches!(current_route, Some(Route::Wallet(_)));
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let helper = AnimationHelper::new(ui, "wallet-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());
        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                notedeck_ui::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let img = if ui.visuals().dark_mode {
            app_images::wallet_dark_image()
        } else {
            app_images::wallet_light_image()
        };
        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );
        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Wallet")
    }
}

fn support_button(current_route: Option<&Route>) -> impl Widget + '_ {
    let is_active = matches!(current_route, Some(Route::Support));
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let helper = AnimationHelper::new(ui, "support-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());
        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                notedeck_ui::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let img = if ui.visuals().dark_mode {
            app_images::help_dark_image()
        } else {
            app_images::help_light_image()
        };
        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );
        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Support")
    }
}

fn home_button() -> impl Widget {
    |ui: &mut egui::Ui| {
        let img_size = 32.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let helper = AnimationHelper::new(ui, "home-button", vec2(max_size, max_size));

        let img = app_images::damus_image();
        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );
        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Home")
    }
}
fn connectivity_indicator(
    ui: &mut egui::Ui,
    pool: &RelayPool,
    _current_route: Option<&Route>,
) -> egui::Response {
    let connected_count = pool
        .relays
        .iter()
        .filter(|r| matches!(r.status(), RelayStatus::Connected))
        .count();
    let total_count = pool.relays.len();

    let indicator_color = if total_count > 1 {
        if connected_count == 0 {
            egui::Color32::from_rgb(0xFF, 0x66, 0x66)
        } else if connected_count == 1 {
            egui::Color32::from_rgb(0xFF, 0xCC, 0x66)
        } else {
            notedeck_ui::side_panel_icon_tint(ui)
        }
    } else {
        notedeck_ui::side_panel_icon_tint(ui)
    };

    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
    let helper = AnimationHelper::new(ui, "connectivity-indicator", vec2(max_size, max_size));

    let painter = ui.painter_at(helper.get_animation_rect());
    let rect = helper.get_animation_rect();
    let center = rect.center();

    let bar_width = 2.0;
    let bar_spacing = 3.0;

    let base_y = center.y + 4.0;
    let start_x = center.x - (bar_width + bar_spacing);

    let bar_heights = [4.0, 7.0, 10.0];
    for (i, &height) in bar_heights.iter().enumerate() {
        let x = start_x + (i as f32) * (bar_width + bar_spacing);
        let bar_rect =
            egui::Rect::from_min_size(egui::pos2(x, base_y - height), vec2(bar_width, height));
        painter.rect_filled(bar_rect, 0.0, indicator_color);
    }

    let count_text = format!("{}", connected_count);
    let font_id = egui::FontId::proportional(10.0);

    painter.text(
        egui::pos2(center.x, center.y - 8.0),
        egui::Align2::CENTER_CENTER,
        count_text,
        font_id,
        indicator_color,
    );

    helper
        .take_animation_response()
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(format!(
            "{}/{} relays connected",
            connected_count, total_count
        ))
}
