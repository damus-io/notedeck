use egui::{Frame, Layout, Margin};
use egui_extras::{Size, StripBuilder};
use enostr::Pubkey;
use nostrdb::Ndb;
use notedeck::{
    ui::is_narrow, ContactState, Images, Localization, MediaJobSender, Router, Settings,
};

use crate::{
    cache::{ConversationCache, ConversationStates},
    nav::{MessagesUiResponse, Route},
    ui::{conversation_header_impl, convo::conversation_ui, nav::render_nav},
};

#[allow(clippy::too_many_arguments)]
pub fn desktop_messages_ui(
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    selected_pubkey: &Pubkey,
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    router: &Router<Route>,
    settings: &Settings,
    contacts: &ContactState,
    i18n: &mut Localization,
) -> MessagesUiResponse {
    let mut nav_resp = None;
    let mut convo_resp = None;

    StripBuilder::new(ui)
        .size(Size::exact(300.0))
        .size(Size::remainder())
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                nav_resp = Some(render_nav(
                    ui,
                    router,
                    settings,
                    cache,
                    states,
                    jobs,
                    ndb,
                    selected_pubkey,
                    img_cache,
                    contacts,
                    i18n,
                ));
            });

            strip.strip(|strip| {
                strip
                    .size(Size::exact(64.0))
                    .size(Size::remainder())
                    .vertical(|mut strip| {
                        strip.cell(|ui| {
                            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                                Frame::new().inner_margin(Margin::symmetric(16, 4)).show(
                                    ui,
                                    |ui| {
                                        conversation_header_impl(
                                            ui,
                                            i18n,
                                            cache,
                                            selected_pubkey,
                                            ndb,
                                            jobs,
                                            img_cache,
                                        );
                                    },
                                );
                            });
                        });
                        strip.cell(|ui| {
                            convo_resp = conversation_ui(
                                cache,
                                states,
                                jobs,
                                ndb,
                                ui,
                                img_cache,
                                i18n,
                                selected_pubkey,
                            );
                        });
                    });
            });
        });

    MessagesUiResponse {
        nav_response: nav_resp,
        conversation_panel_response: convo_resp,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn narrow_messages_ui(
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    selected_pubkey: &Pubkey,
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    router: &Router<Route>,
    settings: &Settings,
    contacts: &ContactState,
    i18n: &mut Localization,
) -> MessagesUiResponse {
    let nav = render_nav(
        ui,
        router,
        settings,
        cache,
        states,
        jobs,
        ndb,
        selected_pubkey,
        img_cache,
        contacts,
        i18n,
    );

    MessagesUiResponse {
        nav_response: Some(nav),
        conversation_panel_response: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn messages_ui(
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    selected_pubkey: &Pubkey,
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    router: &Router<Route>,
    settings: &Settings,
    contacts: &ContactState,
    i18n: &mut Localization,
) -> MessagesUiResponse {
    if is_narrow(ui.ctx()) {
        narrow_messages_ui(
            cache,
            states,
            jobs,
            ndb,
            selected_pubkey,
            ui,
            img_cache,
            router,
            settings,
            contacts,
            i18n,
        )
    } else {
        desktop_messages_ui(
            cache,
            states,
            jobs,
            ndb,
            selected_pubkey,
            ui,
            img_cache,
            router,
            settings,
            contacts,
            i18n,
        )
    }
}
