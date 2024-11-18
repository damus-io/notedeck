use egui::{
    vec2, Color32, InnerResponse, Layout, Margin, RichText, ScrollArea, Separator, Stroke, Widget,
};
use tracing::info;

use crate::{
    account_manager::{AccountManager, AccountsRoute},
    app::get_active_columns_mut,
    app_style::DECK_ICON_SIZE,
    colors,
    column::Column,
    decks::{AccountId, DecksCache},
    imgcache::ImageCache,
    nav::SelectionResponse,
    route::Route,
    support::Support,
    user_account::UserAccount,
    Damus,
};

use super::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    configure_deck::deck_icon,
    profile::preview::get_account_url,
    ProfilePic, View,
};

pub static SIDE_PANEL_WIDTH: f32 = 64.0;
static ICON_WIDTH: f32 = 40.0;

pub struct DesktopSidePanel<'a> {
    ndb: &'a nostrdb::Ndb,
    img_cache: &'a mut ImageCache,
    selected_account: Option<&'a UserAccount>,
    decks_cache: &'a DecksCache,
}

impl<'a> View for DesktopSidePanel<'a> {
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
        img_cache: &'a mut ImageCache,
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
        egui::Frame::none()
            .inner_margin(Margin::same(8.0))
            .show(ui, |ui| self.show_inner(ui))
            .inner
    }

    fn show_inner(&mut self, ui: &mut egui::Ui) -> SidePanelResponse {
        let dark_mode = ui.ctx().style().visuals.dark_mode;

        let inner = ui
            .vertical(|ui| {
                let top_resp = ui
                    .with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        let expand_resp = ui.add(expand_side_panel_button());
                        ui.add_space(28.0);
                        let compose_resp = ui.add(compose_note_button());
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

                        let support_resp = ui.add(support_button());

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
        accounts: &AccountManager,
        support: &mut Support,
        action: SidePanelAction,
    ) -> Option<SelectionResponse> {
        let router = get_active_columns_mut(accounts, decks_cache).get_first_router();
        let mut switching_response = None;
        match action {
            SidePanelAction::Panel => {} // TODO
            SidePanelAction::Account => {
                if router
                    .routes()
                    .iter()
                    .any(|&r| r == Route::Accounts(AccountsRoute::Accounts))
                {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::accounts());
                }
            }
            SidePanelAction::Settings => {
                if router.routes().iter().any(|&r| r == Route::Relays) {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::relays());
                }
            }
            SidePanelAction::Columns => {
                if router.routes().iter().any(|&r| r == Route::AddColumn) {
                    router.go_back();
                } else {
                    get_active_columns_mut(accounts, decks_cache).new_column_picker();
                }
            }
            SidePanelAction::ComposeNote => {
                if router.routes().iter().any(|&r| r == Route::ComposeNote) {
                    router.go_back();
                } else {
                    router.route_to(Route::ComposeNote);
                }
            }
            SidePanelAction::Search => {
                // TODO
                info!("Clicked search button");
            }
            SidePanelAction::ExpandSidePanel => {
                // TODO
                info!("Clicked expand side panel button");
            }
            SidePanelAction::Support => {
                if router.routes().iter().any(|&r| r == Route::Support) {
                    router.go_back();
                } else {
                    support.refresh();
                    router.route_to(Route::Support);
                }
            }
            SidePanelAction::NewDeck => {
                if router.routes().iter().any(|&r| r == Route::NewDeck) {
                    router.go_back();
                } else {
                    router.route_to(Route::NewDeck);
                }
            }
            SidePanelAction::SwitchDeck(index) => {
                switching_response = Some(SelectionResponse::SelectDeck(index))
            }
            SidePanelAction::EditDeck(index) => {
                if router.routes().iter().any(|&r| r == Route::EditDeck(index)) {
                    router.go_back();
                } else {
                    router.route_to(Route::EditDeck(index));
                }
            }
        }
        switching_response
    }
}

fn settings_button(dark_mode: bool) -> impl Widget {
    let _ = dark_mode;
    |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = egui::include_image!("../../assets/icons/settings_dark_4x.png");
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
    let _ = dark_mode;
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let img_data = egui::include_image!("../../assets/icons/add_column_dark_4x.png");

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

fn compose_note_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let min_outer_circle_diameter = 40.0;
        let min_plus_sign_size = 14.0; // length of the plus sign
        let min_line_width = 2.25; // width of the plus sign

        let helper = AnimationHelper::new(ui, "note-compose-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());

        let use_background_radius = helper.scale_radius(min_outer_circle_diameter);
        let use_line_width = helper.scale_1d_pos(min_line_width);
        let use_edge_circle_radius = helper.scale_radius(min_line_width);

        painter.circle_filled(helper.center(), use_background_radius, colors::PINK);

        let min_half_plus_sign_size = min_plus_sign_size / 2.0;
        let north_edge = helper.scale_from_center(0.0, min_half_plus_sign_size);
        let south_edge = helper.scale_from_center(0.0, -min_half_plus_sign_size);
        let west_edge = helper.scale_from_center(-min_half_plus_sign_size, 0.0);
        let east_edge = helper.scale_from_center(min_half_plus_sign_size, 0.0);

        painter.line_segment(
            [north_edge, south_edge],
            Stroke::new(use_line_width, Color32::WHITE),
        );
        painter.line_segment(
            [west_edge, east_edge],
            Stroke::new(use_line_width, Color32::WHITE),
        );
        painter.circle_filled(north_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(south_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(west_edge, use_edge_circle_radius, Color32::WHITE);
        painter.circle_filled(east_edge, use_edge_circle_radius, Color32::WHITE);

        helper.take_animation_response()
    }
}

fn search_button() -> impl Widget {
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
        let img_data = egui::include_image!("../../assets/damus_rounded_80.png");
        let img = egui::Image::new(img_data).max_width(img_size);

        ui.add(img)
    }
}

fn support_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 16.0;

        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = egui::include_image!("../../assets/icons/help_icon_dark_4x.png");
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
        let img_data = egui::include_image!("../../assets/icons/new_deck_icon_4x_dark.png");
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

fn show_decks<'a>(
    ui: &mut egui::Ui,
    decks_cache: &'a DecksCache,
    selected_account: Option<&'a UserAccount>,
) -> InnerResponse<Option<usize>> {
    let show_decks_id = ui.id().with("show-decks");
    let account_id = if let Some(acc) = selected_account {
        AccountId::User(acc.pubkey)
    } else {
        AccountId::Unnamed(0)
    };
    let (cur_decks, account_id) = (
        decks_cache.decks(&account_id),
        show_decks_id.with(&account_id),
    );
    let active_index = cur_decks.active_index();

    let (_, mut resp) = ui.allocate_exact_size(vec2(0.0, 0.0), egui::Sense::click());
    let mut clicked_index = None;
    for (index, deck) in cur_decks.decks().iter().enumerate() {
        let highlight = index == active_index;
        let deck_icon_resp = ui.add(deck_icon(
            account_id.with(index),
            Some(deck.icon),
            DECK_ICON_SIZE,
            40.0,
            highlight,
        ));
        if deck_icon_resp.clicked() || deck_icon_resp.secondary_clicked() {
            clicked_index = Some(index);
        }
        resp = resp.union(deck_icon_resp);
    }
    InnerResponse::new(clicked_index, resp)
}

mod preview {

    use egui_extras::{Size, StripBuilder};

    use crate::{
        app::get_active_columns_mut,
        test_data,
        ui::{Preview, PreviewConfig},
    };

    use super::*;

    pub struct DesktopSidePanelPreview {
        app: Damus,
    }

    impl DesktopSidePanelPreview {
        fn new() -> Self {
            let mut app = test_data::test_app();
            get_active_columns_mut(&app.accounts, &mut app.decks_cache)
                .add_column(Column::new(vec![Route::accounts()]));
            DesktopSidePanelPreview { app }
        }
    }

    impl View for DesktopSidePanelPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            StripBuilder::new(ui)
                .size(Size::exact(SIDE_PANEL_WIDTH))
                .sizes(Size::remainder(), 0)
                .clip(true)
                .horizontal(|mut strip| {
                    strip.cell(|ui| {
                        let mut panel = DesktopSidePanel::new(
                            &self.app.ndb,
                            &mut self.app.img_cache,
                            self.app.accounts.get_selected_account(),
                            &self.app.decks_cache,
                        );
                        let response = panel.show(ui);

                        DesktopSidePanel::perform_action(
                            &mut self.app.decks_cache,
                            &self.app.accounts,
                            &mut self.app.support,
                            response.action,
                        );
                    });
                });
        }
    }

    impl<'a> Preview for DesktopSidePanel<'a> {
        type Prev = DesktopSidePanelPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            DesktopSidePanelPreview::new()
        }
    }
}
