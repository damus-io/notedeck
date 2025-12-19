use chrono::Local;
use egui::{
    Align, Color32, CornerRadius, Frame, Label, Layout, Margin, RichText, ScrollArea, Sense,
};
use egui_extras::{Size, Strip, StripBuilder};
use enostr::Pubkey;
use nostrdb::{Ndb, Note, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, tr, ui::is_narrow, Images, Localization, MediaJobSender,
    NotedeckTextStyle,
};
use notedeck_ui::ProfilePic;

use crate::{
    cache::{
        Conversation, ConversationCache, ConversationId, ConversationState, ConversationStates,
    },
    nav::MessagesAction,
    ui::{
        conversation_title, convo::format_time_short, direct_chat_partner, local_datetime,
        ConversationSummary,
    },
};

pub struct ConversationListUi<'a> {
    cache: &'a ConversationCache,
    states: &'a mut ConversationStates,
    jobs: &'a MediaJobSender,
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    i18n: &'a mut Localization,
}

impl<'a> ConversationListUi<'a> {
    pub fn new(
        cache: &'a ConversationCache,
        states: &'a mut ConversationStates,
        jobs: &'a MediaJobSender,
        ndb: &'a Ndb,
        img_cache: &'a mut Images,
        i18n: &'a mut Localization,
    ) -> Self {
        Self {
            cache,
            states,
            ndb,
            jobs,
            img_cache,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, selected_pubkey: &Pubkey) -> Option<MessagesAction> {
        let mut action = None;
        if self.cache.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(tr!(
                    self.i18n,
                    "No conversations yet",
                    "Empty state text when the user has no conversations"
                ));
            });
            return None;
        }

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let num_convos = self.cache.len();

                self.states
                    .convos_list
                    .ui_custom_layout(ui, num_convos, |ui, index| {
                        let Some(id) = self.cache.get_id_by_index(index).copied() else {
                            return 1;
                        };

                        let Some(convo) = self.cache.get(id) else {
                            return 1;
                        };

                        let state = self.states.cache.get(&id);

                        if let Some(a) = render_list_item(
                            ui,
                            self.ndb,
                            self.cache.active,
                            id,
                            convo,
                            state,
                            self.jobs,
                            self.img_cache,
                            selected_pubkey,
                            self.i18n,
                        ) {
                            action = Some(a);
                        }

                        1
                    });
            });
        action
    }
}

#[allow(clippy::too_many_arguments)]
fn render_list_item(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    active: Option<ConversationId>,
    id: ConversationId,
    convo: &Conversation,
    state: Option<&ConversationState>,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    selected_pubkey: &Pubkey,
    i18n: &mut Localization,
) -> Option<MessagesAction> {
    let txn = Transaction::new(ndb).expect("txn");
    let summary = ConversationSummary::new(convo, state.and_then(|s| s.last_read));

    let title = conversation_title(summary.metadata, &txn, ndb, selected_pubkey, i18n);

    let partner = direct_chat_partner(summary.metadata.participants.as_slice(), selected_pubkey);
    let partner_profile = partner.and_then(|pk| ndb.get_profile_by_pubkey(&txn, pk.bytes()).ok());

    let last_msg = summary
        .last_message
        .and_then(|r| ndb.get_note_by_key(&txn, r.key).ok());

    let response = render_summary(
        ui,
        summary,
        active == Some(id),
        title.as_ref(),
        partner.is_some(),
        last_msg.as_ref(),
        partner_profile.as_ref(),
        jobs,
        img_cache,
        i18n,
    );

    response.clicked().then_some(MessagesAction::Open(id))
}

#[allow(clippy::too_many_arguments)]
pub fn render_summary(
    ui: &mut egui::Ui,
    summary: ConversationSummary,
    selected: bool,
    title: &str,
    show_partner_avatar: bool,
    last_message: Option<&Note>,
    partner_profile: Option<&ProfileRecord<'_>>,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    i18n: &mut Localization,
) -> egui::Response {
    let visuals = ui.visuals();
    let fill = if is_narrow(ui.ctx()) {
        Color32::TRANSPARENT
    } else if selected {
        visuals.extreme_bg_color
    } else if summary.unread {
        visuals.faint_bg_color
    } else {
        Color32::TRANSPARENT
    };

    Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            render_summary_inner(
                ui,
                title,
                show_partner_avatar,
                last_message,
                partner_profile,
                jobs,
                img_cache,
                i18n,
            );
        })
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[allow(clippy::too_many_arguments)]
fn render_summary_inner(
    ui: &mut egui::Ui,
    title: &str,
    show_partner_avatar: bool,
    last_message: Option<&Note>,
    partner_profile: Option<&ProfileRecord<'_>>,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    i18n: &mut Localization,
) {
    let summary_height = 40.0;
    StripBuilder::new(ui)
        .size(Size::exact(summary_height))
        .vertical(|mut strip| {
            strip.strip(|builder| {
                builder
                    .size(Size::exact(summary_height + 8.0))
                    .size(Size::remainder())
                    .horizontal(|strip| {
                        render_summary_horizontal(
                            title,
                            show_partner_avatar,
                            last_message,
                            partner_profile,
                            jobs,
                            img_cache,
                            summary_height,
                            i18n,
                            strip,
                        );
                    });
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn render_summary_horizontal(
    title: &str,
    show_partner_avatar: bool,
    last_message: Option<&Note>,
    partner_profile: Option<&ProfileRecord<'_>>,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    summary_height: f32,
    i18n: &mut Localization,
    mut strip: Strip,
) {
    if show_partner_avatar {
        strip.cell(|ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let size = ProfilePic::default_size() as f32;
                let mut pic = ProfilePic::from_profile_or_default(img_cache, jobs, partner_profile)
                    .size(size);
                ui.add(&mut pic);
            });
        });
    } else {
        strip.empty();
    }

    let title_height = 8.0;
    strip.cell(|ui| {
        StripBuilder::new(ui)
            .size(Size::exact(title_height))
            .size(Size::exact(summary_height - title_height))
            .vertical(|strip| {
                render_summary_body(title, last_message, i18n, strip);
            });
    });
}

fn render_summary_body(
    title: &str,
    last_message: Option<&Note>,
    i18n: &mut Localization,
    mut strip: Strip,
) {
    strip.cell(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(last_msg) = last_message {
                let today = Local::now().date_naive();
                let last_msg_ts = i64::try_from(last_msg.created_at()).unwrap_or(i64::MAX);
                let time_str = format_time_short(today, &local_datetime(last_msg_ts), i18n);

                ui.add_enabled(
                    false,
                    Label::new(
                        RichText::new(time_str)
                            .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4)),
                    ),
                );
            }

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(title).strong())
                        .truncate()
                        .selectable(false),
                );
            });
        });
    });

    let Some(last_msg) = last_message else {
        strip.empty();
        return;
    };

    strip.cell(|ui| {
        ui.add_enabled(
            false, // disables hover & makes text grayed out
            Label::new(
                RichText::new(last_msg.content())
                    .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Body)),
            )
            .truncate(),
        );
    });
}
