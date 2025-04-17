use core::f32;
use std::collections::HashMap;

use egui::{
    pos2, vec2, Align, Color32, FontId, Id, ImageSource, Margin, Pos2, Rect, RichText, Separator,
    Ui, Vec2, Widget,
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use tracing::error;

use crate::{
    login_manager::AcquireKeyState,
    route::Route,
    timeline::{kind::ListKind, PubkeySource, TimelineKind},
    Damus,
};

use notedeck::{AppContext, Images, NotedeckTextStyle, UserAccount};
use notedeck_ui::anim::ICON_EXPANSION_MULTIPLE;
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

use crate::ui::widgets::styled_button;
use notedeck_ui::{anim::AnimationHelper, padding, ProfilePreview};

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

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
pub enum AddAlgoRoute {
    #[default]
    Base,
    LastPerPubkey,
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
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
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    cur_account: Option<&'a UserAccount>,
}

impl<'a> AddColumnView<'a> {
    pub fn new(
        key_state_map: &'a mut HashMap<Id, AcquireKeyState>,
        ndb: &'a Ndb,
        img_cache: &'a mut Images,
        cur_account: Option<&'a UserAccount>,
    ) -> Self {
        Self {
            key_state_map,
            ndb,
            img_cache,
            cur_account,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_base_options() {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = self.cur_account.map(|acct| option.take_as_response(acct))
            }

            ui.add(Separator::default().spacing(0.0));
        }

        selected_option
    }

    fn notifications_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_notifications_options() {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = self.cur_account.map(|acct| option.take_as_response(acct));
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
            title: "Contact List",
            description: "Source the last note for each user in your contact list",
            icon: egui::include_image!("../../../../assets/icons/home_icon_dark_4x.png"),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Decided(
                ListKind::contact_list(deck_author),
            ))),
        };

        let option = algo_option.option.clone();
        if self.column_option_ui(ui, algo_option).clicked() {
            self.cur_account.map(|acct| option.take_as_response(acct))
        } else {
            None
        }
    }

    fn algo_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let algo_option = ColumnOptionData {
            title: "Last Note per User",
            description: "Show the last note for each user from a list",
            icon: egui::include_image!("../../../../assets/icons/algo.png"),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Undecided)),
        };

        let option = algo_option.option.clone();
        if self.column_option_ui(ui, algo_option).clicked() {
            self.cur_account.map(|acct| option.take_as_response(acct))
        } else {
            None
        }
    }

    fn individual_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_individual_options() {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = self.cur_account.map(|acct| option.take_as_response(acct));
            }

            ui.add(Separator::default().spacing(0.0));
        }

        selected_option
    }

    fn external_individual_ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        let id = ui.id().with("external_individual");

        self.external_ui(ui, id, |pubkey| {
            AddColumnOption::Individual(PubkeySource::Explicit(pubkey))
        })
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
                        RichText::new("Enter the user's key (npub, hex, nip05) here...")
                            .text_style(NotedeckTextStyle::Body.text_style()),
                    )
                    .vertical_align(Align::Center)
                    .desired_width(f32::INFINITY)
                    .min_size(Vec2::new(0.0, 40.0))
                    .margin(Margin::same(12))
            });

            ui.add(text_edit);

            key_state.handle_input_change_after_acquire();
            key_state.loading_and_error_ui(ui);

            if key_state.get_login_keypair().is_none() && ui.add(find_user_button()).clicked() {
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
                                ProfilePreview::new(&profile, self.img_cache).ui(ui);
                            });
                    }
                }

                if ui.add(add_column_button()).clicked() {
                    self.cur_account
                        .map(|acc| to_option(keypair.pubkey).take_as_response(acc))
                } else {
                    None
                }
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

        let helper = AnimationHelper::new(ui, data.title, vec2(max_width, max_height));
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
        let icon_img = egui::Image::new(data.icon).fit_to_exact_size(cur_icon_size);
        let icon_rect = Rect::from_center_size(pos2(cur_icon_x_pos, icon_cur_y), cur_icon_size);

        icon_img.paint_at(ui, icon_rect);
        painter.galley(title_corner_pos, title_galley, text_color);
        painter.galley(desc_corner_pos, desc_galley, fallback_color);

        helper.take_animation_response()
    }

    fn get_base_options(&self) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();
        vec.push(ColumnOptionData {
            title: "Universe",
            description: "See the whole nostr universe",
            icon: egui::include_image!("../../../../assets/icons/universe_icon_dark_4x.png"),
            option: AddColumnOption::Universe,
        });

        if let Some(acc) = self.cur_account {
            let source = if acc.key.secret_key.is_some() {
                PubkeySource::DeckAuthor
            } else {
                PubkeySource::Explicit(acc.key.pubkey)
            };

            vec.push(ColumnOptionData {
                title: "Contacts",
                description: "See notes from your contacts",
                icon: egui::include_image!("../../../../assets/icons/home_icon_dark_4x.png"),
                option: AddColumnOption::Contacts(source),
            });
        }
        vec.push(ColumnOptionData {
            title: "Notifications",
            description: "Stay up to date with notifications and mentions",
            icon: egui::include_image!("../../../../assets/icons/notifications_icon_dark_4x.png"),
            option: AddColumnOption::UndecidedNotification,
        });
        vec.push(ColumnOptionData {
            title: "Hashtag",
            description: "Stay up to date with a certain hashtag",
            icon: egui::include_image!("../../../../assets/icons/hashtag_icon_4x.png"),
            option: AddColumnOption::UndecidedHashtag,
        });
        vec.push(ColumnOptionData {
            title: "Individual",
            description: "Stay up to date with someone's notes & replies",
            icon: egui::include_image!("../../../../assets/icons/profile_icon_4x.png"),
            option: AddColumnOption::UndecidedIndividual,
        });
        vec.push(ColumnOptionData {
            title: "Algo",
            description: "Algorithmic feeds to aid in note discovery",
            icon: egui::include_image!("../../../../assets/icons/algo.png"),
            option: AddColumnOption::Algo(AlgoOption::LastPerPubkey(Decision::Undecided)),
        });

        vec
    }

    fn get_notifications_options(&self) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();

        if let Some(acc) = self.cur_account {
            let source = if acc.key.secret_key.is_some() {
                PubkeySource::DeckAuthor
            } else {
                PubkeySource::Explicit(acc.key.pubkey)
            };

            vec.push(ColumnOptionData {
                title: "Your Notifications",
                description: "Stay up to date with your notifications and mentions",
                icon: egui::include_image!(
                    "../../../../assets/icons/notifications_icon_dark_4x.png"
                ),
                option: AddColumnOption::Notification(source),
            });
        }

        vec.push(ColumnOptionData {
            title: "Someone else's Notifications",
            description: "Stay up to date with someone else's notifications and mentions",
            icon: egui::include_image!("../../../../assets/icons/notifications_icon_dark_4x.png"),
            option: AddColumnOption::ExternalNotification,
        });

        vec
    }

    fn get_individual_options(&self) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();

        if let Some(acc) = self.cur_account {
            let source = if acc.key.secret_key.is_some() {
                PubkeySource::DeckAuthor
            } else {
                PubkeySource::Explicit(acc.key.pubkey)
            };

            vec.push(ColumnOptionData {
                title: "Your Notes",
                description: "Keep track of your notes & replies",
                icon: egui::include_image!("../../../../assets/icons/profile_icon_4x.png"),
                option: AddColumnOption::Individual(source),
            });
        }

        vec.push(ColumnOptionData {
            title: "Someone else's Notes",
            description: "Stay up to date with someone else's notes & replies",
            icon: egui::include_image!("../../../../assets/icons/profile_icon_4x.png"),
            option: AddColumnOption::ExternalIndividual,
        });

        vec
    }
}

fn find_user_button() -> impl Widget {
    styled_button("Find User", notedeck_ui::colors::PINK)
}

fn add_column_button() -> impl Widget {
    styled_button("Add", notedeck_ui::colors::PINK)
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
    title: &'static str,
    description: &'static str,
    icon: ImageSource<'static>,
    option: AddColumnOption,
}

pub fn render_add_column_routes(
    ui: &mut egui::Ui,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    col: usize,
    route: &AddColumnRoute,
) {
    let mut add_column_view = AddColumnView::new(
        &mut app.view_state.id_state_map,
        ctx.ndb,
        ctx.img_cache,
        ctx.accounts.get_selected_account(),
    );
    let resp = match route {
        AddColumnRoute::Base => add_column_view.ui(ui),
        AddColumnRoute::Algo(r) => match r {
            AddAlgoRoute::Base => add_column_view.algo_ui(ui),
            AddAlgoRoute::LastPerPubkey => {
                if let Some(deck_author) = ctx.accounts.get_selected_account() {
                    add_column_view.algo_last_per_pk_ui(ui, deck_author.key.pubkey)
                } else {
                    None
                }
            }
        },
        AddColumnRoute::UndecidedNotification => add_column_view.notifications_ui(ui),
        AddColumnRoute::ExternalNotification => add_column_view.external_notification_ui(ui),
        AddColumnRoute::Hashtag => hashtag_ui(ui, &mut app.view_state.id_string_map),
        AddColumnRoute::UndecidedIndividual => add_column_view.individual_ui(ui),
        AddColumnRoute::ExternalIndividual => add_column_view.external_individual_ui(ui),
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
                    &mut app.subscriptions,
                    ctx.pool,
                    ctx.note_cache,
                    app.since_optimize,
                );

                app.columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to_replaced(Route::timeline(timeline.kind.clone()));

                app.timeline_cache
                    .timelines
                    .insert(timeline.kind.clone(), timeline);
            }

            AddColumnResponse::Algo(algo_option) => match algo_option {
                // If we are undecided, we simply route to the LastPerPubkey
                // algo route selection
                AlgoOption::LastPerPubkey(Decision::Undecided) => {
                    app.columns_mut(ctx.accounts)
                        .column_mut(col)
                        .router_mut()
                        .route_to(Route::AddColumn(AddColumnRoute::Algo(
                            AddAlgoRoute::LastPerPubkey,
                        )));
                }

                // We have a decision on where we want the last per pubkey
                // source to be, so let;s create a timeline from that and
                // add it to our list of timelines
                AlgoOption::LastPerPubkey(Decision::Decided(list_kind)) => {
                    let maybe_timeline = {
                        let txn = Transaction::new(ctx.ndb).unwrap();
                        TimelineKind::last_per_pubkey(list_kind).into_timeline(&txn, ctx.ndb)
                    };

                    if let Some(mut timeline) = maybe_timeline {
                        crate::timeline::setup_new_timeline(
                            &mut timeline,
                            ctx.ndb,
                            &mut app.subscriptions,
                            ctx.pool,
                            ctx.note_cache,
                            app.since_optimize,
                        );

                        app.columns_mut(ctx.accounts)
                            .column_mut(col)
                            .router_mut()
                            .route_to_replaced(Route::timeline(timeline.kind.clone()));

                        app.timeline_cache
                            .timelines
                            .insert(timeline.kind.clone(), timeline);
                    } else {
                        // we couldn't fetch the timeline yet... let's let
                        // the user know ?

                        // TODO: spin off the list search here instead

                        ui.label(format!("error: could not find {:?}", list_kind));
                    }
                }
            },

            AddColumnResponse::UndecidedNotification => {
                app.columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(Route::AddColumn(AddColumnRoute::UndecidedNotification));
            }
            AddColumnResponse::ExternalNotification => {
                app.columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(
                        AddColumnRoute::ExternalNotification,
                    ));
            }
            AddColumnResponse::Hashtag => {
                app.columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(AddColumnRoute::Hashtag));
            }
            AddColumnResponse::UndecidedIndividual => {
                app.columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .route_to(crate::route::Route::AddColumn(
                        AddColumnRoute::UndecidedIndividual,
                    ));
            }
            AddColumnResponse::ExternalIndividual => {
                app.columns_mut(ctx.accounts)
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
    id_string_map: &mut HashMap<Id, String>,
) -> Option<AddColumnResponse> {
    padding(16.0, ui, |ui| {
        let id = ui.id().with("hashtag)");
        let text_buffer = id_string_map.entry(id).or_default();

        let text_edit = egui::TextEdit::singleline(text_buffer)
            .hint_text(
                RichText::new("Enter the desired hashtag here")
                    .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(Align::Center)
            .desired_width(f32::INFINITY)
            .min_size(Vec2::new(0.0, 40.0))
            .margin(Margin::same(12));
        ui.add(text_edit);

        ui.add_space(8.0);
        if ui
            .add_sized(egui::vec2(50.0, 40.0), add_column_button())
            .clicked()
        {
            let resp =
                AddColumnResponse::Timeline(TimelineKind::Hashtag(sanitize_hashtag(text_buffer)));
            id_string_map.remove(&id);
            Some(resp)
        } else {
            None
        }
    })
    .inner
}

fn sanitize_hashtag(raw_hashtag: &str) -> String {
    raw_hashtag.replace("#", "")
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
            let mut parser = TokenParser::new(&data);
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
