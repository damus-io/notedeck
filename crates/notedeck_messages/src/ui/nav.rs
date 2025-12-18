use egui::{CornerRadius, CursorIcon, Frame, Margin, Sense, Stroke};
use egui_nav::{NavResponse, RouteResponse};
use enostr::Pubkey;
use nostrdb::Ndb;
use notedeck::{
    tr, ui::is_narrow, ContactState, Images, Localization, MediaJobSender, Router, Settings,
};
use notedeck_ui::{
    app_images,
    header::{chevron, HorizontalHeader},
};

use crate::{
    cache::{ConversationCache, ConversationStates},
    nav::{MessagesAction, Route},
    ui::{
        conversation_header_impl, convo::conversation_ui, convo_list::ConversationListUi,
        create_convo::CreateConvoUi, title_label,
    },
};

#[allow(clippy::too_many_arguments)]
pub fn render_nav(
    ui: &mut egui::Ui,
    router: &Router<Route>,
    settings: &Settings,
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    selected_pubkey: &Pubkey,
    img_cache: &mut Images,
    contacts: &ContactState,
    i18n: &mut Localization,
) -> NavResponse<Option<MessagesAction>> {
    ui.painter().rect(
        ui.available_rect_before_wrap(),
        CornerRadius::ZERO,
        ui.visuals().faint_bg_color,
        Stroke::NONE,
        egui::StrokeKind::Inside,
    );

    if cfg!(target_os = "macos") {
        ui.add_space(16.0);
    }

    egui_nav::Nav::new(router.routes())
        .navigating(router.navigating)
        .returning(router.returning)
        .animate_transitions(settings.animate_nav_transitions)
        .show_mut(ui, |ui, render_type, nav| match render_type {
            egui_nav::NavUiType::Title => {
                let mut nav_title = NavTitle::new(
                    nav.routes(),
                    cache,
                    jobs,
                    ndb,
                    selected_pubkey,
                    img_cache,
                    i18n,
                );
                let response = nav_title.show(ui);

                RouteResponse {
                    response,
                    can_take_drag_from: Vec::new(),
                }
            }
            egui_nav::NavUiType::Body => {
                let Some(top) = nav.routes().last() else {
                    return RouteResponse {
                        response: None,
                        can_take_drag_from: Vec::new(),
                    };
                };

                render_nav_body(
                    top,
                    cache,
                    states,
                    jobs,
                    ndb,
                    selected_pubkey,
                    ui,
                    img_cache,
                    contacts,
                    i18n,
                )
            }
        })
}

#[allow(clippy::too_many_arguments)]
fn render_nav_body(
    top: &Route,
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    selected_pubkey: &Pubkey,
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    contacts: &ContactState,
    i18n: &mut Localization,
) -> RouteResponse<Option<MessagesAction>> {
    let response = match top {
        Route::ConvoList => {
            let mut frame = Frame::new();
            if !is_narrow(ui.ctx()) {
                frame = frame.inner_margin(Margin {
                    left: 12,
                    right: 12,
                    top: 0,
                    bottom: 10,
                });
            }
            frame
                .show(ui, |ui| {
                    ConversationListUi::new(cache, states, jobs, ndb, img_cache, i18n)
                        .ui(ui, selected_pubkey)
                })
                .inner
        }
        Route::CreateConvo => 's: {
            let Some(r) = CreateConvoUi::new(ndb, jobs, img_cache, contacts, i18n).ui(ui) else {
                break 's None;
            };

            Some(MessagesAction::Create {
                recipient: r.recipient,
            })
        }
        Route::Conversation => conversation_ui(
            cache,
            states,
            jobs,
            ndb,
            ui,
            img_cache,
            i18n,
            selected_pubkey,
        ),
    };

    RouteResponse {
        response,
        can_take_drag_from: vec![],
    }
}

pub struct NavTitle<'a> {
    routes: &'a [Route],
    cache: &'a ConversationCache,
    jobs: &'a MediaJobSender,
    ndb: &'a Ndb,
    selected_pubkey: &'a Pubkey,
    img_cache: &'a mut Images,
    i18n: &'a mut Localization,
}

impl<'a> NavTitle<'a> {
    pub fn new(
        routes: &'a [Route],
        cache: &'a ConversationCache,
        jobs: &'a MediaJobSender,
        ndb: &'a Ndb,
        selected_pubkey: &'a Pubkey,
        img_cache: &'a mut Images,
        i18n: &'a mut Localization,
    ) -> Self {
        Self {
            routes,
            cache,
            jobs,
            ndb,
            selected_pubkey,
            img_cache,
            i18n,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<MessagesAction> {
        self.title_bar(ui)
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) -> Option<MessagesAction> {
        let top = self.routes.last()?;

        let mut right_action = None;
        let mut left_action = None;

        HorizontalHeader::new(48.0)
            .with_margin(Margin::symmetric(12, 8))
            .ui(
                ui,
                0,
                1,
                2,
                |ui: &mut egui::Ui| {
                    let chev_width = 12.0;
                    left_action = if prev(self.routes).is_some() {
                        back_button(ui, egui::vec2(chev_width, 20.0))
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .clicked()
                            .then_some(MessagesAction::Back)
                    } else {
                        ui.add(app_images::damus_image().max_width(32.0))
                            .interact(Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .clicked()
                            .then_some(MessagesAction::ToggleChrome)
                    }
                },
                |ui| {
                    self.title(ui, top);
                },
                |ui: &mut egui::Ui| match top {
                    Route::ConvoList => {
                        let new_msg_icon = app_images::new_message_image().max_height(24.0);
                        if ui
                            .add(new_msg_icon)
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .interact(egui::Sense::click())
                            .clicked()
                        {
                            tracing::info!("CLICKED NEW MSG");
                            right_action = Some(MessagesAction::Creating);
                        }
                    }
                    Route::CreateConvo => {}
                    Route::Conversation => {}
                },
            );

        right_action.or(left_action)
    }

    fn title(&mut self, ui: &mut egui::Ui, route: &Route) {
        match route {
            Route::ConvoList => {
                let label = tr!(
                    self.i18n,
                    "Chats",
                    "Title for the list of chat conversations"
                );
                title_label(ui, &label);
            }
            Route::CreateConvo => {
                let label = tr!(
                    self.i18n,
                    "New Chat",
                    "Title shown when composing a new conversation"
                );
                title_label(ui, &label);
            }
            Route::Conversation => self.conversation_title_section(ui),
        }
    }

    fn conversation_title_section(&mut self, ui: &mut egui::Ui) {
        conversation_header_impl(
            ui,
            self.i18n,
            self.cache,
            self.selected_pubkey,
            self.ndb,
            self.jobs,
            self.img_cache,
        );
    }
}

fn back_button(ui: &mut egui::Ui, chev_size: egui::Vec2) -> egui::Response {
    let color = ui.style().visuals.noninteractive().fg_stroke.color;
    chevron(ui, 2.0, chev_size, egui::Stroke::new(2.0, color))
}

fn prev<R>(xs: &[R]) -> Option<&R> {
    xs.get(xs.len().checked_sub(2)?)
}
