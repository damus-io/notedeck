use egui::{
    vec2, Button, Color32, InnerResponse, Label, Layout, Margin, RichText, ScrollArea, Separator,
    Stroke, ThemePreference, Widget,
};
use tracing::{error, info};

use crate::{
    accounts::AccountsRoute,
    app::{get_active_columns_mut, get_decks_mut},
    app_style::DECK_ICON_SIZE,
    colors,
    decks::{DecksAction, DecksCache},
    nav::SwitchingAction,
    route::Route,
    support::Support,
};

use notedeck::{Accounts, Images, NotedeckTextStyle, ThemeHandler, UserAccount};

use super::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    configure_deck::deck_icon,
    profile::preview::get_account_url,
    ProfilePic, View,
};

pub static SIDE_PANEL_WIDTH: f32 = 68.0;
static ICON_WIDTH: f32 = 40.0;

pub struct DesktopSidePanel<'a> {
    ndb: &'a nostrdb::Ndb,
    img_cache: &'a mut Images,
    selected_account: Option<&'a UserAccount>,
    decks_cache: &'a DecksCache,
}

impl View for DesktopSidePanel<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SidePanelAction {
    Panel,
    Account,
    Settings,
    Columns,
    ComposeNote,
    Search,
    ExpandSidePanel,
    Support,
    NewDeck,
    SwitchDeck(usize),
    EditDeck(usize),
    SaveTheme(ThemePreference),
    Wallet,
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
        ndb: &'a nostrdb::Ndb,
        img_cache: &'a mut Images,
        selected_account: Option<&'a UserAccount>,
        decks_cache: &'a DecksCache,
    ) -> Self {
        Self {
            ndb,
            img_cache,
            selected_account,
            decks_cache,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> SidePanelResponse {
        let mut frame = egui::Frame::new().inner_margin(Margin::same(8));

        if !ui.visuals().dark_mode {
            frame = frame.fill(colors::ALMOST_WHITE);
        }

        frame.show(ui, |ui| self.show_inner(ui)).inner
    }

    fn show_inner(&mut self, ui: &mut egui::Ui) -> SidePanelResponse {
        let dark_mode = ui.ctx().style().visuals.dark_mode;

        let inner = ui
            .vertical(|ui| {
                let top_resp = ui
                    .with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        // macos needs a bit of space to make room for window
                        // minimize/close buttons
                        if cfg!(target_os = "macos") {
                            ui.add_space(24.0);
                        }

                        let expand_resp = ui.add(expand_side_panel_button());
                        ui.add_space(4.0);
                        ui.add(milestone_name());
                        ui.add_space(16.0);
                        let is_interactive = self
                            .selected_account
                            .is_some_and(|s| s.key.secret_key.is_some());
                        let compose_resp = ui.add(compose_note_button(is_interactive, dark_mode));
                        let compose_resp = if is_interactive {
                            compose_resp
                        } else {
                            compose_resp.on_hover_cursor(egui::CursorIcon::NotAllowed)
                        };
                        let search_resp = ui.add(search_button());
                        let column_resp = ui.add(add_column_button(dark_mode));

                        ui.add(Separator::default().horizontal().spacing(8.0).shrink(4.0));

                        ui.add_space(8.0);
                        ui.add(egui::Label::new(
                            RichText::new("DECKS")
                                .size(11.0)
                                .color(ui.visuals().noninteractive().fg_stroke.color),
                        ));
                        ui.add_space(8.0);
                        let add_deck_resp = ui.add(add_deck_button());

                        let decks_inner = ScrollArea::vertical()
                            .max_height(ui.available_height() - (3.0 * (ICON_WIDTH + 12.0)))
                            .show(ui, |ui| {
                                show_decks(ui, self.decks_cache, self.selected_account)
                            })
                            .inner;
                        if expand_resp.clicked() {
                            Some(InnerResponse::new(
                                SidePanelAction::ExpandSidePanel,
                                expand_resp,
                            ))
                        } else if compose_resp.clicked() {
                            Some(InnerResponse::new(
                                SidePanelAction::ComposeNote,
                                compose_resp,
                            ))
                        } else if search_resp.clicked() {
                            Some(InnerResponse::new(SidePanelAction::Search, search_resp))
                        } else if column_resp.clicked() {
                            Some(InnerResponse::new(SidePanelAction::Columns, column_resp))
                        } else if add_deck_resp.clicked() {
                            Some(InnerResponse::new(SidePanelAction::NewDeck, add_deck_resp))
                        } else if decks_inner.response.secondary_clicked() {
                            info!("decks inner secondary click");
                            if let Some(clicked_index) = decks_inner.inner {
                                Some(InnerResponse::new(
                                    SidePanelAction::EditDeck(clicked_index),
                                    decks_inner.response,
                                ))
                            } else {
                                None
                            }
                        } else if decks_inner.response.clicked() {
                            if let Some(clicked_index) = decks_inner.inner {
                                Some(InnerResponse::new(
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
                    .inner;

                ui.add(Separator::default().horizontal().spacing(8.0).shrink(4.0));
                let (pfp_resp, bottom_resp) = ui
                    .with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                        let pfp_resp = self.pfp_button(ui);
                        let settings_resp = ui.add(settings_button(dark_mode));

                        let save_theme = if let Some((theme, resp)) = match ui.ctx().theme() {
                            egui::Theme::Dark => {
                                let resp = ui
                                    .add(Button::new("☀").frame(false))
                                    .on_hover_text("Switch to light mode");
                                if resp.clicked() {
                                    Some((ThemePreference::Light, resp))
                                } else {
                                    None
                                }
                            }
                            egui::Theme::Light => {
                                let resp = ui
                                    .add(Button::new("🌙").frame(false))
                                    .on_hover_text("Switch to dark mode");
                                if resp.clicked() {
                                    Some((ThemePreference::Dark, resp))
                                } else {
                                    None
                                }
                            }
                        } {
                            ui.ctx().set_theme(theme);
                            Some((theme, resp))
                        } else {
                            None
                        };

                        let support_resp = ui.add(support_button());

                        let wallet_resp = ui.add(wallet_button());

                        let optional_inner = if pfp_resp.clicked() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Account,
                                pfp_resp.clone(),
                            ))
                        } else if settings_resp.clicked() || settings_resp.hovered() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Settings,
                                settings_resp,
                            ))
                        } else if support_resp.clicked() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Support,
                                support_resp,
                            ))
                        } else if let Some((theme, resp)) = save_theme {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::SaveTheme(theme),
                                resp,
                            ))
                        } else if wallet_resp.clicked() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Wallet,
                                wallet_resp,
                            ))
                        } else {
                            None
                        };

                        (pfp_resp, optional_inner)
                    })
                    .inner;

                if let Some(bottom_inner) = bottom_resp {
                    bottom_inner
                } else if let Some(top_inner) = top_resp {
                    top_inner
                } else {
                    egui::InnerResponse::new(SidePanelAction::Panel, pfp_resp)
                }
            })
            .inner;

        SidePanelResponse::new(inner.inner, inner.response)
    }

    fn pfp_button(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let helper = AnimationHelper::new(ui, "pfp-button", vec2(max_size, max_size));

        let min_pfp_size = ICON_WIDTH;
        let cur_pfp_size = helper.scale_1d_pos(min_pfp_size);

        let txn = nostrdb::Transaction::new(self.ndb).expect("should be able to create txn");
        let profile_url = get_account_url(&txn, self.ndb, self.selected_account);

        let widget = ProfilePic::new(self.img_cache, profile_url).size(cur_pfp_size);

        ui.put(helper.get_animation_rect(), widget);

        helper.take_animation_response()
    }

    pub fn perform_action(
        decks_cache: &mut DecksCache,
        accounts: &Accounts,
        support: &mut Support,
        theme_handler: &mut ThemeHandler,
        action: SidePanelAction,
    ) -> Option<SwitchingAction> {
        let router = get_active_columns_mut(accounts, decks_cache).get_first_router();
        let mut switching_response = None;
        match action {
            SidePanelAction::Panel => {} // TODO
            SidePanelAction::Account => {
                if router
                    .routes()
                    .iter()
                    .any(|r| r == &Route::Accounts(AccountsRoute::Accounts))
                {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::accounts());
                }
            }
            SidePanelAction::Settings => {
                if router.routes().iter().any(|r| r == &Route::Relays) {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::relays());
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
                    get_active_columns_mut(accounts, decks_cache).new_column_picker();
                }
            }
            SidePanelAction::ComposeNote => {
                if router.routes().iter().any(|r| r == &Route::ComposeNote) {
                    router.go_back();
                } else {
                    router.route_to(Route::ComposeNote);
                }
            }
            SidePanelAction::Search => {
                // TODO
                if router.top() == &Route::Search {
                    router.go_back();
                } else {
                    router.route_to(Route::Search);
                }
            }
            SidePanelAction::ExpandSidePanel => {
                // TODO
                info!("Clicked expand side panel button");
            }
            SidePanelAction::Support => {
                if router.routes().iter().any(|r| r == &Route::Support) {
                    router.go_back();
                } else {
                    support.refresh();
                    router.route_to(Route::Support);
                }
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
                    if let Some(edit_deck) = get_decks_mut(accounts, decks_cache)
                        .decks_mut()
                        .get_mut(index)
                    {
                        edit_deck
                            .columns_mut()
                            .get_first_router()
                            .route_to(Route::EditDeck(index));
                    } else {
                        error!("Cannot push EditDeck route to index {}", index);
                    }
                }
            }
            SidePanelAction::SaveTheme(theme) => {
                theme_handler.save(theme);
            }
            SidePanelAction::Wallet => 's: {
                if router.routes().iter().any(|r| r == &Route::Wallet) {
                    router.go_back();
                    break 's;
                }

                router.route_to(Route::Wallet);
            }
        }
        switching_response
    }
}

fn settings_button(dark_mode: bool) -> impl Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = if dark_mode {
            egui::include_image!("../../../../assets/icons/settings_dark_4x.png")
        } else {
            egui::include_image!("../../../../assets/icons/settings_light_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "settings-button", vec2(max_size, max_size));

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

fn add_column_button(dark_mode: bool) -> impl Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let img_data = if dark_mode {
            egui::include_image!("../../../../assets/icons/add_column_dark_4x.png")
        } else {
            egui::include_image!("../../../../assets/icons/add_column_light_4x.png")
        };

        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "add-column-button", vec2(max_size, max_size));

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

fn compose_note_button(interactive: bool, dark_mode: bool) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let min_outer_circle_diameter = 40.0;
        let min_plus_sign_size = 14.0; // length of the plus sign
        let min_line_width = 2.25; // width of the plus sign

        let helper = if interactive {
            AnimationHelper::new(ui, "note-compose-button", vec2(max_size, max_size))
        } else {
            AnimationHelper::no_animation(ui, vec2(max_size, max_size))
        };

        let painter = ui.painter_at(helper.get_animation_rect());

        let use_background_radius = helper.scale_radius(min_outer_circle_diameter);
        let use_line_width = helper.scale_1d_pos(min_line_width);
        let use_edge_circle_radius = helper.scale_radius(min_line_width);

        let fill_color = if interactive {
            colors::PINK
        } else {
            ui.visuals().noninteractive().bg_fill
        };

        painter.circle_filled(helper.center(), use_background_radius, fill_color);

        let min_half_plus_sign_size = min_plus_sign_size / 2.0;
        let north_edge = helper.scale_from_center(0.0, min_half_plus_sign_size);
        let south_edge = helper.scale_from_center(0.0, -min_half_plus_sign_size);
        let west_edge = helper.scale_from_center(-min_half_plus_sign_size, 0.0);
        let east_edge = helper.scale_from_center(min_half_plus_sign_size, 0.0);

        let icon_color = if !dark_mode && !interactive {
            Color32::BLACK
        } else {
            Color32::WHITE
        };

        painter.line_segment(
            [north_edge, south_edge],
            Stroke::new(use_line_width, icon_color),
        );
        painter.line_segment(
            [west_edge, east_edge],
            Stroke::new(use_line_width, icon_color),
        );
        painter.circle_filled(north_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(south_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(west_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(east_edge, use_edge_circle_radius, Color32::WHITE);

        helper.take_animation_response()
    }
}

pub fn search_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let min_line_width_circle = 1.5; // width of the magnifying glass
        let min_line_width_handle = 1.5;
        let helper = AnimationHelper::new(ui, "search-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());

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

        let circle_stroke = Stroke::new(cur_line_width_circle, colors::MID_GRAY);
        let handle_stroke = Stroke::new(cur_line_width_handle, colors::MID_GRAY);

        painter.line_segment([handle_pos_1, handle_pos_2], handle_stroke);
        painter.circle(
            circle_center,
            min_outer_circle_radius,
            ui.style().visuals.widgets.inactive.weak_bg_fill,
            circle_stroke,
        );

        helper.take_animation_response()
    }
}

// TODO: convert to responsive button when expanded side panel impl is finished
fn expand_side_panel_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 40.0;
        let img_data = egui::include_image!("../../../../assets/damus_rounded_80.png");
        let img = egui::Image::new(img_data).max_width(img_size);

        ui.add(img)
    }
}

fn support_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 16.0;

        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = if ui.visuals().dark_mode {
            egui::include_image!("../../../../assets/icons/help_icon_dark_4x.png")
        } else {
            egui::include_image!("../../../../assets/icons/help_icon_inverted_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "help-button", vec2(max_size, max_size));

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

fn add_deck_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 40.0;

        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = egui::include_image!("../../../../assets/icons/new_deck_icon_4x_dark.png");
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "new-deck-icon", vec2(max_size, max_size));

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

fn wallet_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 24.0;

        let max_size = img_size * ICON_EXPANSION_MULTIPLE;
        let img_data = egui::include_image!("../../../../assets/icons/wallet-icon.svg");

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

fn show_decks<'a>(
    ui: &mut egui::Ui,
    decks_cache: &'a DecksCache,
    selected_account: Option<&'a UserAccount>,
) -> InnerResponse<Option<usize>> {
    let show_decks_id = ui.id().with("show-decks");
    let account_id = if let Some(acc) = selected_account {
        acc.key.pubkey
    } else {
        *decks_cache.get_fallback_pubkey()
    };
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
            .on_hover_text_at_pointer(&deck.name);
        if deck_icon_resp.clicked() || deck_icon_resp.secondary_clicked() {
            clicked_index = Some(index);
        }
        resp = resp.union(deck_icon_resp);
    }
    InnerResponse::new(clicked_index, resp)
}

fn milestone_name() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        ui.vertical_centered(|ui| {
            let font = egui::FontId::new(
                notedeck::fonts::get_font_size(
                    ui.ctx(),
                    &NotedeckTextStyle::Tiny,
                ),
                egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(Label::new(
                RichText::new("ALPHA")
                    .color( ui.style().visuals.noninteractive().fg_stroke.color)
                    .font(font),
            ).selectable(false)).on_hover_text("Notedeck is an alpha product. Expect bugs and contact us when you run into issues.").on_hover_cursor(egui::CursorIcon::Help)
        })
            .inner
    }
}
