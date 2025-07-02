use egui::{vec2, InnerResponse, Layout, Margin, RichText, ScrollArea, Separator, Stroke, Widget};
use tracing::{error, info};

use crate::{
    app::{get_active_columns_mut, get_decks_mut},
    app_style::DECK_ICON_SIZE,
    decks::{DecksAction, DecksCache},
    nav::SwitchingAction,
    route::Route,
};

use notedeck::{Accounts, UserAccount};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    app_images, colors, View,
};

use super::configure_deck::deck_icon;

pub static SIDE_PANEL_WIDTH: f32 = 68.0;
static ICON_WIDTH: f32 = 40.0;

pub struct DesktopSidePanel<'a> {
    selected_account: &'a UserAccount,
    decks_cache: &'a DecksCache,
}

impl View for DesktopSidePanel<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SidePanelAction {
    Columns,
    ComposeNote,
    Search,
    ExpandSidePanel,
    NewDeck,
    SwitchDeck(usize),
    EditDeck(usize),
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
    pub fn new(selected_account: &'a UserAccount, decks_cache: &'a DecksCache) -> Self {
        Self {
            selected_account,
            decks_cache,
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
        let dark_mode = ui.ctx().style().visuals.dark_mode;

        let inner = ui
            .vertical(|ui| {
                ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                    // macos needs a bit of space to make room for window
                    // minimize/close buttons
                    //if cfg!(target_os = "macos") {
                    //    ui.add_space(24.0);
                    //}

                    let is_interactive = self.selected_account.key.secret_key.is_some();
                    let compose_resp = ui.add(crate::ui::post::compose_note_button(
                        is_interactive,
                        dark_mode,
                    ));
                    let compose_resp = if is_interactive {
                        compose_resp
                    } else {
                        compose_resp.on_hover_cursor(egui::CursorIcon::NotAllowed)
                    };
                    let search_resp = ui.add(search_button());
                    let column_resp = ui.add(add_column_button());

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

                    /*
                    if expand_resp.clicked() {
                        Some(InnerResponse::new(
                            SidePanelAction::ExpandSidePanel,
                            expand_resp,
                        ))
                    */
                    if compose_resp.clicked() {
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
                .inner
            })
            .inner;

        if let Some(inner) = inner {
            Some(SidePanelResponse::new(inner.inner, inner.response))
        } else {
            None
        }
    }

    pub fn perform_action(
        decks_cache: &mut DecksCache,
        accounts: &Accounts,
        action: SidePanelAction,
    ) -> Option<SwitchingAction> {
        let router = get_active_columns_mut(accounts, decks_cache).get_first_router();
        let mut switching_response = None;
        match action {
            /*
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
            SidePanelAction::Support => {
                if router.routes().iter().any(|r| r == &Route::Support) {
                    router.go_back();
                } else {
                    support.refresh();
                    router.route_to(Route::Support);
                }
            }
            */
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

fn add_deck_button() -> impl Widget {
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

        helper.take_animation_response()
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
            .on_hover_text_at_pointer(&deck.name);
        if deck_icon_resp.clicked() || deck_icon_resp.secondary_clicked() {
            clicked_index = Some(index);
        }
        resp = resp.union(deck_icon_resp);
    }
    InnerResponse::new(clicked_index, resp)
}
