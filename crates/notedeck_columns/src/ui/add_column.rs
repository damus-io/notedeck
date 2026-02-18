use core::f32;
use std::collections::HashMap;

use egui::{
    pos2, vec2, Align, Color32, FontId, Id, Image, Margin, Pos2, Rect, RichText, ScrollArea,
    Separator, Ui, Vec2, Widget,
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use tracing::error;

use crate::{
    login_manager::AcquireKeyState,
    options::AppOptions,
    route::Route,
    timeline::{kind::ListKind, PubkeySource, TimelineKind},
    Damus,
};

use notedeck::{
    tr, AppContext, ContactState, Images, Localization, MediaJobSender, NotedeckTextStyle,
    UserAccount,
};
use notedeck_ui::{anim::ICON_EXPANSION_MULTIPLE, app_images};
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

use crate::ui::widgets::styled_button;
use notedeck_ui::{
    anim::AnimationHelper, padding, profile_row, search_input_box, search_profiles,
    ContactsListView, ProfilePreview,
};

pub enum AddColumnResponse {
    Timeline(TimelineKind),
    UndecidedNotification,
    ExternalNotification,
    Hashtag,
    Algo(AlgoOption),
    UndecidedIndividual,
    ExternalIndividual,
}

pub enum NotificationColumnType {
    Contacts,
    External,
}

#[derive(Clone, Debug)]
pub enum Decision<T> {
    Undecided,
    Decided(T),
}

#[derive(Clone, Debug)]
pub enum AlgoOption {
    LastPerPubkey(Decision<ListKind>),
}

#[derive(Clone, Debug)]
enum AddColumnOption {
    Universe,
    UndecidedNotification,
    ExternalNotification,
    Algo(AlgoOption),
    Notification(PubkeySource),
    Contacts(PubkeySource),
    UndecidedHashtag,
    UndecidedIndividual,
    ExternalIndividual,
    Individual(PubkeySource),
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default, Hash)]
pub enum AddAlgoRoute {
    #[default]
    Base,
    LastPerPubkey,
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub enum AddColumnRoute {
    Base,
    UndecidedNotification,
    ExternalNotification,
    Hashtag,
    Algo(AddAlgoRoute),
    UndecidedIndividual,
    ExternalIndividual,
}

// Parser for the common case without any payloads
fn parse_column_route<'a>(
    parser: &mut TokenParser<'a>,
    route: AddColumnRoute,
) -> Result<AddColumnRoute, ParseError<'a>> {
    parser.parse_all(|p| {
        for token in route.tokens() {
            p.parse_token(token)?;
        }
        Ok(route)
    })
}

impl AddColumnRoute {
    /// Route tokens use in both serialization and deserialization
    fn tokens(&self) -> &'static [&'static str] {
        match self {
            Self::Base => &["column"],
            Self::UndecidedNotification => &["column", "notification_selection"],
            Self::ExternalNotification => &["column", "external_notif_selection"],
            Self::UndecidedIndividual => &["column", "individual_selection"],
            Self::ExternalIndividual => &["column", "external_individual_selection"],
            Self::Hashtag => &["column", "hashtag"],
            Self::Algo(AddAlgoRoute::Base) => &["column", "algo_selection"],
            Self::Algo(AddAlgoRoute::LastPerPubkey) => {
                &["column", "algo_selection", "last_per_pubkey"]
            } // NOTE!!! When adding to this, update the parser for TokenSerializable below
        }
    }
}

impl TokenSerializable for AddColumnRoute {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        for token in self.tokens() {
            writer.write_token(token);
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.peek_parse_token("column")?;

        TokenParser::alt(
            parser,
            &[
                |p| parse_column_route(p, AddColumnRoute::Base),
                |p| parse_column_route(p, AddColumnRoute::UndecidedNotification),
                |p| parse_column_route(p, AddColumnRoute::ExternalNotification),
                |p| parse_column_route(p, AddColumnRoute::UndecidedIndividual),
                |p| parse_column_route(p, AddColumnRoute::ExternalIndividual),
                |p| parse_column_route(p, AddColumnRoute::Hashtag),
                |p| parse_column_route(p, AddColumnRoute::Algo(AddAlgoRoute::Base)),
                |p| parse_column_route(p, AddColumnRoute::Algo(AddAlgoRoute::LastPerPubkey)),
            ],
        )
    }
}

impl AddColumnOption {
    pub fn take_as_response(self, cur_account: &UserAccount) -> AddColumnResponse {
        match self {
            AddColumnOption::Algo(algo_option) => AddColumnResponse::Algo(algo_option),
            AddColumnOption::Universe => AddColumnResponse::Timeline(TimelineKind::Universe),
            AddColumnOption::Notification(pubkey) => AddColumnResponse::Timeline(
                TimelineKind::Notifications(*pubkey.as_pubkey(&cur_account.key.pubkey)),
            ),
            AddColumnOption::UndecidedNotification => AddColumnResponse::UndecidedNotification,
            AddColumnOption::Contacts(pk_src) => AddColumnResponse::Timeline(
                TimelineKind::contact_list(*pk_src.as_pubkey(&cur_account.key.pubkey)),
            ),
            AddColumnOption::ExternalNotification => AddColumnResponse::ExternalNotification,
            AddColumnOption::UndecidedHashtag => AddColumnResponse::Hashtag,
            AddColumnOption::UndecidedIndividual => AddColumnResponse::UndecidedIndividual,
            AddColumnOption::ExternalIndividual => AddColumnResponse::ExternalIndividual,
            AddColumnOption::Individual(pubkey_source) => AddColumnResponse::Timeline(
                TimelineKind::profile(*pubkey_source.as_pubkey(&cur_account.key.pubkey)),
            ),
        }
    }
}

pub struct AddColumnView<'a> {
    key_state_map: &'a mut HashMap<Id, AcquireKeyState>,
    id_string_map: &'a mut HashMap<Id, String>,
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    cur_account: &'a UserAccount,
    contacts: &'a ContactState,
    i18n: &'a mut Localization,
    jobs: &'a MediaJobSender,
}

impl<'a> AddColumnView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key_state_map: &'a mut HashMap<Id, AcquireKeyState>,
        id_string_map: &'a mut HashMap<Id, String>,
        ndb: &'a Ndb,
        img_cache: &'a mut Images,
        cur_account: &'a UserAccount,
        contacts: &'a ContactState,
        i18n: &'a mut Localization,
        jobs: &'a MediaJobSender,
    ) -> Self {
        Self {
            key_state_map,
            id_string_map,
            ndb,
            img_cache,
            cur_account,
            contacts,
            i18n,
            jobs,
        }
    }

    pub fn scroll_id(route: &AddColumnRoute) -> egui::Id {
        egui::Id::new(("add_column", route))
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        ScrollArea::vertical()
            .id_salt(AddColumnView::scroll_id(&AddColumnRoute::Base))
            .show(ui, |ui| {
                let mut selected_option: Option<AddColumnResponse> = None;
                for column_option_data in self.get_base_options(ui) {
                    let option = column_option_data.option.clone();
                    if self.column_option_ui(ui, column_option_data).clicked() {
                        selected_option = Some(option.take_as_response(self.cur_account));
                    }

                    ui.add(Separator::default().spacing(0.0));
                }

                selected_option
            })
            .inner
    }

    fn notifications_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_notifications_options(ui) {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = Some(option.take_as_response(self.cur_account));
            }

            ui.add(Separator::default().spacing(0.0));
        }

        selected_option
    }

    fn external_notification_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let id = ui.id().with("external_notif");
        self.external_ui(ui, id, |pubkey| {
            AddColumnOption::Notification(PubkeySource::Explicit(pubkey))
        })
    }

    fn algo_last_per_pk_ui(
        &mut self,
        ui: &mut Ui,
        deck_author: Pubkey,
    ) -> Option<AddColumnResponse> {
        let algo_option = ColumnOptionData {
            title: tr!(self.i18n, "Contact List", "Title for contact list column"),
            description: tr!(
                self.i18n,
                "Source the last note for each user in your contact list",
                "Description for contact list column"
            ),
            icon: app_images::home_image(),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Decided(
                ListKind::contact_list(deck_author),
            ))),
        };

        let option = algo_option.option.clone();
        self.column_option_ui(ui, algo_option)
            .clicked()
            .then(|| option.take_as_response(self.cur_account))
    }

    fn algo_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let algo_option = ColumnOptionData {
            title: tr!(
                self.i18n,
                "Last Note per User",
                "Title for last note per user column"
            ),
            description: tr!(
                self.i18n,
                "Show the last note for each user from a list",
                "Description for last note per user column"
            ),
            icon: app_images::algo_image(),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Undecided)),
        };

        let option = algo_option.option.clone();
        self.column_option_ui(ui, algo_option)
            .clicked()
            .then(|| option.take_as_response(self.cur_account))
    }

    fn individual_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_individual_options() {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = Some(option.take_as_response(self.cur_account));
            }

            ui.add(Separator::default().spacing(0.0));
        }

        selected_option
    }

    fn external_individual_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let id = ui.id().with("external_individual");

        ui.add_space(8.0);
        let hint = tr!(
            self.i18n,
            "Search profiles or enter nip05 address...",
            "Placeholder for profile search input"
        );
        let query_buf = self.id_string_map.entry(id).or_default();
        ui.add(search_input_box(query_buf, &hint));
        ui.add_space(12.0);

        let query = self
            .id_string_map
            .get(&id)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        if query.contains('@') {
            nip05_profile_ui(
                ui,
                id,
                &query,
                self.key_state_map,
                self.ndb,
                self.img_cache,
                self.jobs,
                self.i18n,
                self.cur_account,
            )
        } else if query.is_empty() {
            self.key_state_map.remove(&id);
            contacts_list_column_ui(
                ui,
                self.contacts,
                self.jobs,
                self.ndb,
                self.img_cache,
                self.i18n,
                self.cur_account,
            )
        } else {
            self.key_state_map.remove(&id);
            profile_search_column_ui(
                ui,
                &query,
                self.ndb,
                self.contacts,
                self.img_cache,
                self.jobs,
                self.i18n,
                self.cur_account,
            )
        }
    }

    fn external_ui(
        &mut self,
        ui: &mut Ui,
        id: egui::Id,
        to_option: fn(Pubkey) -> AddColumnOption,
    ) -> Option<AddColumnResponse> {
        padding(16.0, ui, |ui| {
            let key_state = self.key_state_map.entry(id).or_default();

            let text_edit = key_state.get_acquire_textedit(|text| {
                egui::TextEdit::singleline(text)
                    .hint_text(
                        RichText::new(tr!(
                            self.i18n,
                            "Enter the user's key (npub, hex, nip05) here...",
                            "Hint text to prompt entering the user's public key."
                        ))
                        .text_style(NotedeckTextStyle::Body.text_style()),
                    )
                    .vertical_align(Align::Center)
                    .desired_width(f32::INFINITY)
                    .min_size(Vec2::new(0.0, 40.0))
                    .margin(Margin::same(12))
            });

            ui.add(text_edit);

            key_state.handle_input_change_after_acquire();
            key_state.loading_and_error_ui(ui, self.i18n);

            if key_state.get_login_keypair().is_none()
                && ui.add(find_user_button(self.i18n)).clicked()
            {
                key_state.apply_acquire();
            }

            let resp = if let Some(keypair) = key_state.get_login_keypair() {
                {
                    let txn = Transaction::new(self.ndb).expect("txn");
                    if let Ok(profile) =
                        self.ndb.get_profile_by_pubkey(&txn, keypair.pubkey.bytes())
                    {
                        egui::Frame::window(ui.style())
                            .outer_margin(Margin {
                                left: 4,
                                right: 4,
                                top: 12,
                                bottom: 32,
                            })
                            .show(ui, |ui| {
                                ProfilePreview::new(&profile, self.img_cache, self.jobs).ui(ui);
                            });
                    }
                }

                ui.add(add_column_button(self.i18n))
                    .clicked()
                    .then(|| to_option(keypair.pubkey).take_as_response(self.cur_account))
            } else {
                None
            };
            if resp.is_some() {
                self.key_state_map.remove(&id);
            };
            resp
        })
        .inner
    }

    fn column_option_ui(&mut self, ui: &mut Ui, data: ColumnOptionData) -> egui::Response {
        let icon_padding = 8.0;
        let min_icon_width = 32.0;
        let height_padding = 12.0;
        let inter_text_padding = 4.0; // Padding between title and description
        let max_width = ui.available_width();
        let title_style = NotedeckTextStyle::Body;
        let desc_style = NotedeckTextStyle::Button;
        let title_min_font_size = notedeck::fonts::get_font_size(ui.ctx(), &title_style);
        let desc_min_font_size = notedeck::fonts::get_font_size(ui.ctx(), &desc_style);

        let max_height = {
            let max_wrap_width =
                max_width - ((icon_padding * 2.0) + (min_icon_width * ICON_EXPANSION_MULTIPLE));
            let title_max_font = FontId::new(
                title_min_font_size * ICON_EXPANSION_MULTIPLE,
                title_style.font_family(),
            );
            let desc_max_font = FontId::new(
                desc_min_font_size * ICON_EXPANSION_MULTIPLE,
                desc_style.font_family(),
            );
            let max_desc_galley = ui.fonts(|f| {
                f.layout(
                    data.description.to_string(),
                    desc_max_font,
                    ui.style().visuals.noninteractive().fg_stroke.color,
                    max_wrap_width,
                )
            });
            let max_title_galley = ui.fonts(|f| {
                f.layout(
                    data.title.to_string(),
                    title_max_font,
                    Color32::WHITE,
                    max_wrap_width,
                )
            });

            let desc_font_max_size = max_desc_galley.rect.height();
            let title_font_max_size = max_title_galley.rect.height();
            title_font_max_size + inter_text_padding + desc_font_max_size + (2.0 * height_padding)
        };

        let title = data.title.clone();
        let helper = AnimationHelper::new(ui, title.clone(), vec2(max_width, max_height));
        let animation_rect = helper.get_animation_rect();

        let cur_icon_width = helper.scale_1d_pos(min_icon_width);
        let painter = ui.painter_at(animation_rect);

        let cur_icon_size = vec2(cur_icon_width, cur_icon_width);
        let cur_icon_x_pos = animation_rect.left() + icon_padding + (cur_icon_width / 2.0);

        let title_cur_font = FontId::new(
            helper.scale_1d_pos(title_min_font_size),
            title_style.font_family(),
        );
        let desc_cur_font = FontId::new(
            helper.scale_1d_pos(desc_min_font_size),
            desc_style.font_family(),
        );

        let wrap_width = max_width - (cur_icon_width + (icon_padding * 2.0));
        let text_color = ui.style().visuals.text_color();
        let fallback_color = ui.style().visuals.noninteractive().fg_stroke.color;

        let title_galley = painter.layout(
            data.title.to_string(),
            title_cur_font,
            text_color,
            wrap_width,
        );
        let desc_galley = painter.layout(
            data.description.to_string(),
            desc_cur_font,
            fallback_color,
            wrap_width,
        );

        let total_content_height =
            title_galley.rect.height() + inter_text_padding + desc_galley.rect.height();
        let cur_height_padding = (animation_rect.height() - total_content_height) / 2.0;
        let corner_x_pos = cur_icon_x_pos + (cur_icon_width / 2.0) + icon_padding;
        let title_corner_pos = Pos2::new(corner_x_pos, animation_rect.top() + cur_height_padding);
        let desc_corner_pos = Pos2::new(
            corner_x_pos,
            title_corner_pos.y + title_galley.rect.height() + inter_text_padding,
        );

        let icon_cur_y = animation_rect.top() + cur_height_padding + (total_content_height / 2.0);
        let icon_img = data.icon.fit_to_exact_size(cur_icon_size);
        let icon_rect = Rect::from_center_size(pos2(cur_icon_x_pos, icon_cur_y), cur_icon_size);

        icon_img.paint_at(ui, icon_rect);
        painter.galley(title_corner_pos, title_galley, text_color);
        painter.galley(desc_corner_pos, desc_galley, fallback_color);

        helper.take_animation_response()
    }

    fn get_base_options(&mut self, ui: &mut Ui) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Home", "Title for Home column"),
            description: tr!(
                self.i18n,
                "See notes from your contacts",
                "Description for Home column"
            ),
            icon: app_images::home_image(),
            option: AddColumnOption::Contacts(if self.cur_account.key.secret_key.is_some() {
                PubkeySource::DeckAuthor
            } else {
                PubkeySource::Explicit(self.cur_account.key.pubkey)
            }),
        });
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Notifications", "Title for notifications column"),
            description: tr!(
                self.i18n,
                "Stay up to date with notifications and mentions",
                "Description for notifications column"
            ),
            icon: app_images::notifications_image(ui.visuals().dark_mode),
            option: AddColumnOption::UndecidedNotification,
        });
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Universe", "Title for universe column"),
            description: tr!(
                self.i18n,
                "See the whole nostr universe",
                "Description for universe column"
            ),
            icon: app_images::universe_image(),
            option: AddColumnOption::Universe,
        });
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Hashtags", "Title for hashtags column"),
            description: tr!(
                self.i18n,
                "Stay up to date with a certain hashtag",
                "Description for hashtags column"
            ),
            icon: app_images::hashtag_image(),
            option: AddColumnOption::UndecidedHashtag,
        });
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Individual", "Title for individual user column"),
            description: tr!(
                self.i18n,
                "Stay up to date with someone's notes & replies",
                "Description for individual user column"
            ),
            icon: app_images::add_column_individual_image(),
            option: AddColumnOption::UndecidedIndividual,
        });
        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Algo", "Title for algorithmic feeds column"),
            description: tr!(
                self.i18n,
                "Algorithmic feeds to aid in note discovery",
                "Description for algorithmic feeds column"
            ),
            icon: app_images::algo_image(),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Undecided)),
        });

        vec
    }

    fn get_notifications_options(&mut self, ui: &mut Ui) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();

        let source = if self.cur_account.key.secret_key.is_some() {
            PubkeySource::DeckAuthor
        } else {
            PubkeySource::Explicit(self.cur_account.key.pubkey)
        };

        vec.push(ColumnOptionData {
            title: tr!(
                self.i18n,
                "Your Notifications",
                "Title for your notifications column"
            ),
            description: tr!(
                self.i18n,
                "Stay up to date with your notifications and mentions",
                "Description for your notifications column"
            ),
            icon: app_images::notifications_image(ui.visuals().dark_mode),
            option: AddColumnOption::Notification(source),
        });

        vec.push(ColumnOptionData {
            title: tr!(
                self.i18n,
                "Someone else's Notifications",
                "Title for someone else's notifications column"
            ),
            description: tr!(
                self.i18n,
                "Stay up to date with someone else's notifications and mentions",
                "Description for someone else's notifications column"
            ),
            icon: app_images::notifications_image(ui.visuals().dark_mode),
            option: AddColumnOption::ExternalNotification,
        });

        vec
    }

    fn get_individual_options(&mut self) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();

        let source = if self.cur_account.key.secret_key.is_some() {
            PubkeySource::DeckAuthor
        } else {
            PubkeySource::Explicit(self.cur_account.key.pubkey)
        };

        vec.push(ColumnOptionData {
            title: tr!(self.i18n, "Your Notes", "Title for your notes column"),
            description: tr!(
                self.i18n,
                "Keep track of your notes & replies",
                "Description for your notes column"
            ),
            icon: app_images::add_column_individual_image(),
            option: AddColumnOption::Individual(source),
        });

        vec.push(ColumnOptionData {
            title: tr!(
                self.i18n,
                "Someone else's Notes",
                "Title for someone else's notes column"
            ),
            description: tr!(
                self.i18n,
                "Stay up to date with someone else's notes & replies",
                "Description for someone else's notes column"
            ),
            icon: app_images::add_column_individual_image(),
            option: AddColumnOption::ExternalIndividual,
        });

        vec
    }
}

fn find_user_button(i18n: &mut Localization) -> impl Widget {
    let label = tr!(i18n, "Find User", "Label for find user button");
    let color = notedeck_ui::colors::PINK;
    move |ui: &mut egui::Ui| styled_button(label.as_str(), color).ui(ui)
}

fn add_column_button(i18n: &mut Localization) -> impl Widget {
    let label = tr!(i18n, "Add", "Label for add column button");
    let color = notedeck_ui::colors::PINK;
    move |ui: &mut egui::Ui| styled_button(label.as_str(), color).ui(ui)
}

fn individual_column_response(pubkey: Pubkey, cur_account: &UserAccount) -> AddColumnResponse {
    AddColumnOption::Individual(PubkeySource::Explicit(pubkey)).take_as_response(cur_account)
}

#[allow(clippy::too_many_arguments)]
fn nip05_profile_ui(
    ui: &mut Ui,
    id: egui::Id,
    query: &str,
    key_state_map: &mut HashMap<Id, AcquireKeyState>,
    ndb: &Ndb,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
    cur_account: &UserAccount,
) -> Option<AddColumnResponse> {
    let key_state = key_state_map.entry(id).or_default();

    // Sync the search input into AcquireKeyState's buffer
    let buf = key_state.input_buffer();
    if *buf != query {
        buf.clear();
        buf.push_str(query);
        key_state.apply_acquire();
    }

    key_state.loading_and_error_ui(ui, i18n);

    let resp = if let Some(keypair) = key_state.get_login_keypair() {
        let txn = Transaction::new(ndb).expect("txn");
        let profile = ndb.get_profile_by_pubkey(&txn, keypair.pubkey.bytes()).ok();

        profile_row(ui, profile.as_ref(), false, img_cache, jobs, i18n)
            .then(|| individual_column_response(keypair.pubkey, cur_account))
    } else {
        None
    };

    if resp.is_some() {
        key_state_map.remove(&id);
    }

    resp
}

fn contacts_list_column_ui(
    ui: &mut Ui,
    contacts: &ContactState,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    img_cache: &mut Images,
    i18n: &mut Localization,
    cur_account: &UserAccount,
) -> Option<AddColumnResponse> {
    let ContactState::Received {
        contacts: contact_set,
        ..
    } = contacts
    else {
        return None;
    };

    let txn = Transaction::new(ndb).expect("txn");
    let resp = ContactsListView::new(contact_set, jobs, ndb, img_cache, &txn, i18n).ui(ui);

    resp.output.map(|a| match a {
        notedeck_ui::ContactsListAction::Select(pubkey) => {
            individual_column_response(pubkey, cur_account)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn profile_search_column_ui(
    ui: &mut Ui,
    query: &str,
    ndb: &Ndb,
    contacts: &ContactState,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
    cur_account: &UserAccount,
) -> Option<AddColumnResponse> {
    let txn = Transaction::new(ndb).expect("txn");
    let results = search_profiles(ndb, &txn, query, contacts, 128);

    if results.is_empty() {
        ui.add_space(20.0);
        ui.label(
            RichText::new(tr!(
                i18n,
                "No profiles found",
                "Shown when profile search returns no results"
            ))
            .weak(),
        );
        return None;
    }

    let mut action = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for result in &results {
            let profile = ndb.get_profile_by_pubkey(&txn, &result.pk).ok();
            if profile_row(
                ui,
                profile.as_ref(),
                result.is_contact,
                img_cache,
                jobs,
                i18n,
            ) {
                action = Some(individual_column_response(
                    Pubkey::new(result.pk),
                    cur_account,
                ));
            }
        }
    });
    action
}

/*
pub(crate) fn sized_button(text: &str) -> impl Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let galley = painter.layout(
            text.to_owned(),
            NotedeckTextStyle::Body.get_font_id(ui.ctx()),
            Color32::WHITE,
            ui.available_width(),
        );

        ui.add_sized(
            galley.rect.expand2(vec2(16.0, 8.0)).size(),
            egui::Button::new(galley)
                .corner_radius(8.0)
                .fill(notedeck_ui::colors::PINK),
        )
    }
}
*/

struct ColumnOptionData {
    title: String,
    description: String,
    icon: Image<'static>,
    option: AddColumnOption,
}

pub fn render_add_column_routes(
    ui: &mut egui::Ui,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    col: usize,
    route: &AddColumnRoute,
) {
    // Handle hashtag separately since it borrows id_string_map directly
    let resp = if matches!(route, AddColumnRoute::Hashtag) {
        hashtag_ui(ui, ctx.i18n, &mut app.view_state.id_string_map)
    } else {
        let account = ctx.accounts.get_selected_account();
        let contacts = account.data.contacts.get_state();
        let mut add_column_view = AddColumnView::new(
            &mut app.view_state.id_state_map,
            &mut app.view_state.id_string_map,
            ctx.ndb,
            ctx.img_cache,
            account,
            contacts,
            ctx.i18n,
            ctx.media_jobs.sender(),
        );
        match route {
            AddColumnRoute::Base => add_column_view.ui(ui),
            AddColumnRoute::Algo(r) => match r {
                AddAlgoRoute::Base => add_column_view.algo_ui(ui),
                AddAlgoRoute::LastPerPubkey => {
                    add_column_view.algo_last_per_pk_ui(ui, account.key.pubkey)
                }
            },
            AddColumnRoute::UndecidedNotification => add_column_view.notifications_ui(ui),
            AddColumnRoute::ExternalNotification => add_column_view.external_notification_ui(ui),
            AddColumnRoute::Hashtag => unreachable!(),
            AddColumnRoute::UndecidedIndividual => add_column_view.individual_ui(ui),
            AddColumnRoute::ExternalIndividual => add_column_view.external_individual_ui(ui),
        }
    };

    if let Some(resp) = resp {
        match resp {
            AddColumnResponse::Timeline(timeline_kind) => 'leave: {
                let txn = Transaction::new(ctx.ndb).unwrap();
                let mut timeline =
                    if let Some(timeline) = timeline_kind.into_timeline(&txn, ctx.ndb) {
                        timeline
                    } else {
                        error!("Could not convert column response to timeline");
                        break 'leave;
                    };

                crate::timeline::setup_new_timeline(
                    &mut timeline,
                    ctx.ndb,
                    &txn,
                    &mut app.subscriptions,
                    ctx.pool,
                    ctx.note_cache,
                    app.options.contains(AppOptions::SinceOptimize),
                    ctx.accounts,
                    ctx.unknown_ids,
                );

                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to_replaced(Route::timeline(timeline.kind.clone()));

                app.timeline_cache.insert(timeline.kind.clone(), timeline);
            }

            AddColumnResponse::Algo(algo_option) => match algo_option {
                // If we are undecided, we simply route to the LastPerPubkey
                // algo route selection
                AlgoOption::LastPerPubkey(Decision::Undecided) => {
                    app.columns_mut(ctx.i18n, ctx.accounts)
                        .column_mut(col)
                        .router_mut()
                        .route_to(Route::AddColumn(AddColumnRoute::Algo(
                            AddAlgoRoute::LastPerPubkey,
                        )));
                }

                // We have a decision on where we want the last per pubkey
                // source to be, so let's create a timeline from that and
                // add it to our list of timelines
                AlgoOption::LastPerPubkey(Decision::Decided(list_kind)) => {
                    let txn = Transaction::new(ctx.ndb).unwrap();
                    let maybe_timeline =
                        TimelineKind::last_per_pubkey(list_kind).into_timeline(&txn, ctx.ndb);

                    if let Some(mut timeline) = maybe_timeline {
                        crate::timeline::setup_new_timeline(
                            &mut timeline,
                            ctx.ndb,
                            &txn,
                            &mut app.subscriptions,
                            ctx.pool,
                            ctx.note_cache,
                            app.options.contains(AppOptions::SinceOptimize),
                            ctx.accounts,
                            ctx.unknown_ids,
                        );

                        app.columns_mut(ctx.i18n, ctx.accounts)
                            .column_mut(col)
                            .router_mut()
                            .route_to_replaced(Route::timeline(timeline.kind.clone()));

                        app.timeline_cache.insert(timeline.kind.clone(), timeline);
                    } else {
                        // we couldn't fetch the timeline yet... let's let
                        // the user know ?

                        // TODO: spin off the list search here instead

                        ui.label(format!("error: could not find {list_kind:?}"));
                    }
                }
            },

            AddColumnResponse::UndecidedNotification => {
                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(Route::AddColumn(AddColumnRoute::UndecidedNotification));
            }
            AddColumnResponse::ExternalNotification => {
                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(
                        AddColumnRoute::ExternalNotification,
                    ));
            }
            AddColumnResponse::Hashtag => {
                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(AddColumnRoute::Hashtag));
            }
            AddColumnResponse::UndecidedIndividual => {
                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(
                        AddColumnRoute::UndecidedIndividual,
                    ));
            }
            AddColumnResponse::ExternalIndividual => {
                app.columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(
                        AddColumnRoute::ExternalIndividual,
                    ));
            }
        };
    }
}

pub fn hashtag_ui(
    ui: &mut Ui,
    i18n: &mut Localization,
    id_string_map: &mut HashMap<Id, String>,
) -> Option<AddColumnResponse> {
    padding(16.0, ui, |ui| {
        let id = ui.id().with("hashtag)");
        let text_buffer = id_string_map.entry(id).or_default();

        let text_edit = egui::TextEdit::singleline(text_buffer)
            .hint_text(
                RichText::new(tr!(
                    i18n,
                    "Enter the desired hashtags here (for multiple space-separated)",
                    "Placeholder for hashtag input field"
                ))
                .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(Align::Center)
            .desired_width(f32::INFINITY)
            .min_size(Vec2::new(0.0, 40.0))
            .margin(Margin::same(12));
        ui.add(text_edit);

        ui.add_space(8.0);

        let mut handle_user_input = false;
        if ui.input(|i| i.key_released(egui::Key::Enter))
            || ui
                .add_sized(egui::vec2(50.0, 40.0), add_column_button(i18n))
                .clicked()
        {
            handle_user_input = true;
        }

        if handle_user_input && !text_buffer.is_empty() {
            let resp = AddColumnResponse::Timeline(TimelineKind::Hashtag(
                text_buffer
                    .split_whitespace()
                    .filter(|s| !s.is_empty())
                    .map(|s| sanitize_hashtag(s).to_lowercase().to_string())
                    .collect::<Vec<_>>(),
            ));
            id_string_map.remove(&id);
            Some(resp)
        } else {
            None
        }
    })
    .inner
}

fn sanitize_hashtag(raw_hashtag: &str) -> String {
    raw_hashtag
        .chars()
        .filter(|c| c.is_alphanumeric()) // keep letters and numbers only
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_serialize() {
        use super::{AddAlgoRoute, AddColumnRoute};

        {
            let data_str = "column:algo_selection:last_per_pubkey";
            let data = &data_str.split(":").collect::<Vec<&str>>();
            let mut token_writer = TokenWriter::default();
            let mut parser = TokenParser::new(data);
            let parsed = AddColumnRoute::parse_from_tokens(&mut parser).unwrap();
            let expected = AddColumnRoute::Algo(AddAlgoRoute::LastPerPubkey);
            parsed.serialize_tokens(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }

        {
            let data_str = "column";
            let mut token_writer = TokenWriter::default();
            let data: &[&str] = &[data_str];
            let mut parser = TokenParser::new(data);
            let parsed = AddColumnRoute::parse_from_tokens(&mut parser).unwrap();
            let expected = AddColumnRoute::Base;
            parsed.serialize_tokens(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }
    }
}
