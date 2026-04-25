use egui::{Frame, Layout, Margin};
use egui_extras::{Size, StripBuilder};
use egui_winit::clipboard::Clipboard;
use enostr::Pubkey;
use nostrdb::Ndb;
use notedeck::{
    ui::is_narrow, ContactState, Images, Localization, MediaJobSender, Router, Settings,
};

use crate::{
    cache::{ConversationCache, ConversationStates},
    nav::{MessagesUiResponse, Route},
    ui::{
        conversation_details_button, conversation_header_impl, convo::conversation_ui,
        nav::render_nav, show_conversation_details_modal, MessagesTransportStatus,
    },
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
    clipboard: &mut Clipboard,
) -> MessagesUiResponse {
    let mut nav_resp = None;
    let mut convo_resp = None;
    let mut header_resp = None;

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
                    clipboard,
                ));
            });

            strip.strip(|strip| {
                strip
                    .size(Size::exact(64.0))
                    .size(Size::remainder())
                    .vertical(|mut strip| {
                        strip.cell(|ui| {
                            Frame::new()
                                .inner_margin(Margin::symmetric(16, 4))
                                .show(ui, |ui| {
                                    StripBuilder::new(ui)
                                        .size(Size::remainder())
                                        .size(Size::exact(36.0))
                                        .horizontal(|mut strip| {
                                            strip.cell(|ui| {
                                                ui.with_layout(
                                                    Layout::left_to_right(egui::Align::Center),
                                                    |ui| {
                                                        header_resp = conversation_header_impl(
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
                                            strip.cell(|ui| {
                                                ui.with_layout(
                                                    Layout::right_to_left(egui::Align::Center),
                                                    |ui| {
                                                        if cache.get_active().is_some()
                                                            && conversation_details_button(ui)
                                                                .clicked()
                                                        {
                                                            header_resp = Some(
                                                                crate::nav::MessagesAction::ShowConversationDetails,
                                                            );
                                                        }
                                                    },
                                                );
                                            });
                                        });
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
                                clipboard,
                            );
                        });
                    });
            });
        });

    MessagesUiResponse {
        nav_response: nav_resp,
        conversation_panel_response: convo_resp.or(header_resp),
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
    clipboard: &mut Clipboard,
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
        clipboard,
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
    clipboard: &mut Clipboard,
    transport: MessagesTransportStatus,
) -> MessagesUiResponse {
    let response = if is_narrow(ui.ctx()) {
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
            clipboard,
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
            clipboard,
        )
    };

    show_conversation_details_modal(ui, cache, states, ndb, transport, i18n);
    response
}
