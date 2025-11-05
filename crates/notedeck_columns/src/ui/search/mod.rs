use egui::{vec2, Align, Color32, CornerRadius, Key, RichText, Stroke, TextEdit};
use enostr::{NoteId, Pubkey};
use state::TypingType;

use crate::{
    nav::BodyResponse,
    timeline::{TimelineTab, TimelineUnits},
    ui::{timeline::TimelineTabView, widgets::UserRow},
};
use egui_winit::clipboard::Clipboard;
use nostrdb::{Filter, Ndb, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, name::get_display_name, profile::get_profile_url, tr, tr_plural,
    Images, JobsCache, Localization, NoteAction, NoteContext, NoteRef, NotedeckTextStyle,
};

use notedeck_ui::{
    context_menu::{input_context, PasteBehavior},
    icons::search_icon,
    padding, NoteOptions, ProfilePic,
};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

mod state;

pub use state::{FocusState, RecentSearchItem, SearchQueryState, SearchState};

use super::mentions_picker::{MentionPickerResponse, MentionPickerView};

pub struct SearchView<'a, 'd> {
    query: &'a mut SearchQueryState,
    note_options: NoteOptions,
    txn: &'a Transaction,
    note_context: &'a mut NoteContext<'d>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> SearchView<'a, 'd> {
    pub fn new(
        txn: &'a Transaction,
        note_options: NoteOptions,
        query: &'a mut SearchQueryState,
        note_context: &'a mut NoteContext<'d>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        Self {
            txn,
            query,
            note_options,
            note_context,
            jobs,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> BodyResponse<NoteAction> {
        padding(8.0, ui, |ui| self.show_impl(ui)).inner.map_output(|action| match action {
            SearchViewAction::NoteAction(note_action) => note_action,
            SearchViewAction::NavigateToProfile(pubkey) => NoteAction::Profile(pubkey),
        })
    }

    fn show_impl(&mut self, ui: &mut egui::Ui) -> BodyResponse<SearchViewAction> {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 12.0);

        let search_resp = search_box(
            self.note_context.i18n,
            &mut self.query.string,
            self.query.focus_state.clone(),
            ui,
            self.note_context.clipboard,
        );

        search_resp.process(self.query);

        let keyboard_resp = handle_keyboard_navigation(ui, &mut self.query.selected_index, &self.query.user_results);

        let mut search_action = None;
        let mut body_resp = BodyResponse::none();
        match &self.query.state {
            SearchState::New | SearchState::Navigating | SearchState::Typing(TypingType::Mention(_)) => {
                if !self.query.string.is_empty() && !self.query.string.starts_with('@') {
                    self.query.user_results = self.note_context.ndb.search_profile(self.txn, &self.query.string, 10)
                        .unwrap_or_default()
                        .iter()
                        .map(|&pk| pk.to_vec())
                        .collect();
                    if let Some(action) = self.show_search_suggestions(ui, keyboard_resp) {
                        search_action = Some(action);
                    }
                } else if self.query.string.starts_with('@') {
                    self.handle_mention_search(ui, &mut search_action);
                } else {
                    self.query.user_results.clear();
                    self.query.selected_index = -1;
                    if let Some(action) = self.show_recent_searches(ui, keyboard_resp) {
                        search_action = Some(action);
                    }
                }
            }
            SearchState::PerformSearch(search_type) => {
                execute_search(
                    ui.ctx(),
                    search_type,
                    &self.query.string,
                    self.note_context.ndb,
                    self.txn,
                    &mut self.query.notes,
                );
                search_action = Some(SearchAction::Searched);
                body_resp.insert(self.show_search_results(ui).map_output(SearchViewAction::NoteAction));
            }
            SearchState::Searched => {
                ui.label(tr_plural!(
                    self.note_context.i18n,
                    "Got {count} result for '{query}'",  // one
                    "Got {count} results for '{query}'", // other
                    "Search results count",              // comment
                    self.query.notes.units.len(),        // count
                    query = &self.query.string
                ));
                body_resp.insert(self.show_search_results(ui).map_output(SearchViewAction::NoteAction));
            }
        };

        if let Some(action) = search_action {
            if let Some(view_action) = action.process(self.query) {
                body_resp.output = Some(view_action);
            }
        }

        body_resp
    }

    fn handle_mention_search(&mut self, ui: &mut egui::Ui, search_action: &mut Option<SearchAction>) {
        let mention_name = if let Some(mention_text) = self.query.string.get(1..) {
            mention_text
        } else {
            return;
        };

        's: {
            let Ok(results) = self
                .note_context
                .ndb
                .search_profile(self.txn, mention_name, 10)
            else {
                break 's;
            };

            let search_res = MentionPickerView::new(
                self.note_context.img_cache,
                self.note_context.ndb,
                self.txn,
                &results,
                self.note_context.accounts,
            )
            .show_in_rect(ui.available_rect_before_wrap(), ui);

            let Some(res) = search_res.output else {
                break 's;
            };

            *search_action = match res {
                MentionPickerResponse::SelectResult(Some(index)) => {
                    let Some(pk_bytes) = results.get(index) else {
                        break 's;
                    };

                    let username = self
                        .note_context
                        .ndb
                        .get_profile_by_pubkey(self.txn, pk_bytes)
                        .ok()
                        .and_then(|p| p.record().profile().and_then(|p| p.name()))
                        .unwrap_or(&self.query.string);

                    Some(SearchAction::NewSearch {
                        search_type: SearchType::Profile(Pubkey::new(**pk_bytes)),
                        new_search_text: format!("@{username}"),
                    })
                }
                MentionPickerResponse::DeleteMention => Some(SearchAction::CloseMention),
                MentionPickerResponse::SelectResult(None) => break 's,
            };
        }
    }

    fn show_search_suggestions(&mut self, ui: &mut egui::Ui, keyboard_resp: KeyboardResponse) -> Option<SearchAction> {
        ui.add_space(8.0);

        let is_selected = self.query.selected_index == 0;
        let search_posts_clicked = ui.add(search_posts_button(
            &self.query.string,
            is_selected,
            ui.available_width(),
        )).clicked() || (is_selected && keyboard_resp.enter_pressed);

        if search_posts_clicked {
            return Some(SearchAction::NewSearch {
                search_type: SearchType::get_type(&self.query.string),
                new_search_text: self.query.string.clone(),
            });
        }

        if keyboard_resp.enter_pressed && self.query.selected_index > 0 {
            let user_idx = (self.query.selected_index - 1) as usize;
            if let Some(pk_bytes) = self.query.user_results.get(user_idx) {
                if let Ok(pk_array) = TryInto::<[u8; 32]>::try_into(pk_bytes.as_slice()) {
                    return Some(SearchAction::NavigateToProfile(Pubkey::new(pk_array)));
                }
            }
        }

        if !self.query.user_results.is_empty() {
            ui.add_space(8.0);

            for (i, pk_bytes) in self.query.user_results.iter().enumerate() {
                let Ok(pk_array) = TryInto::<[u8; 32]>::try_into(pk_bytes.as_slice()) else {
                    continue;
                };
                let pubkey = Pubkey::new(pk_array);
                let profile = self.note_context.ndb.get_profile_by_pubkey(self.txn, &pk_array).ok();

                let is_selected = self.query.selected_index == (i as i32 + 1);
                if ui.add(UserRow::new(profile.as_ref(), &pubkey, self.note_context.img_cache, ui.available_width())
                    .with_accounts(self.note_context.accounts)
                    .with_selection(is_selected)).clicked() {
                    return Some(SearchAction::NavigateToProfile(pubkey));
                }
            }
        }

        None
    }

    fn show_recent_searches(&mut self, ui: &mut egui::Ui, keyboard_resp: KeyboardResponse) -> Option<SearchAction> {
        if self.query.recent_searches.is_empty() {
            return None;
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Recent");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Clear all").size(14.0)).clicked() {
                    self.query.clear_recent_searches();
                }
            });
        });
        ui.add_space(4.0);

        let recent_searches = self.query.recent_searches.clone();
        for (i, search_item) in recent_searches.iter().enumerate() {
            let is_selected = self.query.selected_index == i as i32;

            match search_item {
                RecentSearchItem::Query(query) => {
                    let resp = ui.add(recent_search_item(
                        query,
                        is_selected,
                        ui.available_width(),
                        false,
                    ));

                    if resp.clicked() || (is_selected && keyboard_resp.enter_pressed) {
                        return Some(SearchAction::NewSearch {
                            search_type: SearchType::get_type(query),
                            new_search_text: query.clone(),
                        });
                    }

                    if resp.secondary_clicked() || (is_selected && ui.input(|i| i.key_pressed(Key::Delete))) {
                        self.query.remove_recent_search(i);
                    }
                }
                RecentSearchItem::Profile { pubkey, query } => {
                    let profile = self.note_context.ndb.get_profile_by_pubkey(self.txn, pubkey.bytes()).ok();
                    let resp = ui.add(recent_profile_item(
                        profile.as_ref(),
                        pubkey,
                        query,
                        is_selected,
                        ui.available_width(),
                        self.note_context.img_cache,
                        self.note_context.accounts,
                    ));

                    if resp.clicked() || (is_selected && keyboard_resp.enter_pressed) {
                        return Some(SearchAction::NavigateToProfile(*pubkey));
                    }

                    if resp.secondary_clicked() || (is_selected && ui.input(|i| i.key_pressed(Key::Delete))) {
                        self.query.remove_recent_search(i);
                    }
                }
            }
        }

        None
    }

    fn show_search_results(&mut self, ui: &mut egui::Ui) -> BodyResponse<NoteAction> {
        let scroll_out = egui::ScrollArea::vertical()
            .id_salt(SearchView::scroll_id())
            .show(ui, |ui| {
                TimelineTabView::new(
                    &self.query.notes,
                    self.note_options,
                    self.txn,
                    self.note_context,
                    self.jobs,
                )
                .show(ui)
            });

        BodyResponse::scroll(scroll_out)
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("search_results")
    }
}

fn execute_search(
    ctx: &egui::Context,
    search_type: &SearchType,
    raw_input: &String,
    ndb: &Ndb,
    txn: &Transaction,
    tab: &mut TimelineTab,
) {
    if raw_input.is_empty() {
        return;
    }

    let max_results = 500;

    let Some(note_refs) = search_type.search(raw_input, ndb, txn, max_results) else {
        return;
    };

    tab.units = TimelineUnits::from_refs_single(note_refs);
    tab.list.borrow_mut().reset();
    ctx.request_repaint();
}

enum SearchViewAction {
    NoteAction(NoteAction),
    NavigateToProfile(Pubkey),
}

enum SearchAction {
    NewSearch {
        search_type: SearchType,
        new_search_text: String,
    },
    NavigateToProfile(Pubkey),
    Searched,
    CloseMention,
}

impl SearchAction {
    fn process(self, state: &mut SearchQueryState) -> Option<SearchViewAction> {
        match self {
            SearchAction::NewSearch {
                search_type,
                new_search_text,
            } => {
                state.state = SearchState::PerformSearch(search_type);
                state.string = new_search_text;
                state.selected_index = -1;
                None
            }
            SearchAction::NavigateToProfile(pubkey) => {
                state.add_recent_profile(pubkey, state.string.clone());
                state.string.clear();
                state.selected_index = -1;
                Some(SearchViewAction::NavigateToProfile(pubkey))
            }
            SearchAction::CloseMention => {
                state.state = SearchState::New;
                state.selected_index = -1;
                None
            }
            SearchAction::Searched => {
                state.state = SearchState::Searched;
                state.selected_index = -1;
                state.user_results.clear();
                state.add_recent_query(state.string.clone());
                None
            }
        }
    }
}

struct SearchResponse {
    requested_focus: bool,
    input_changed: bool,
}

impl SearchResponse {
    fn process(self, state: &mut SearchQueryState) {
        if self.requested_focus {
            state.focus_state = FocusState::RequestedFocus;
        } else if state.focus_state == FocusState::RequestedFocus && !self.input_changed {
            state.focus_state = FocusState::Navigating;
        }

        if self.input_changed {
            if state.string.starts_with('@') {
                state.selected_index = -1;
                if let Some(mention_text) = state.string.get(1..) {
                    state.state = SearchState::Typing(TypingType::Mention(mention_text.to_owned()));
                }
            } else if state.state == SearchState::Searched {
                state.state = SearchState::New;
                state.selected_index = 0;
            } else if !state.string.is_empty() {
                state.selected_index = 0;
            } else {
                state.selected_index = -1;
            }
        }
    }
}

struct KeyboardResponse {
    enter_pressed: bool,
}

fn handle_keyboard_navigation(ui: &mut egui::Ui, selected_index: &mut i32, user_results: &[Vec<u8>]) -> KeyboardResponse {
    let max_index = if user_results.is_empty() {
        -1
    } else {
        user_results.len() as i32
    };

    if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
        *selected_index = (*selected_index + 1).min(max_index);
    } else if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
        *selected_index = (*selected_index - 1).max(-1);
    }

    let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));

    KeyboardResponse { enter_pressed }
}

fn search_box(
    i18n: &mut Localization,
    input: &mut String,
    focus_state: FocusState,
    ui: &mut egui::Ui,
    clipboard: &mut Clipboard,
) -> SearchResponse {
    ui.horizontal(|ui| {
        // Container for search input and icon
        let search_container = egui::Frame {
            inner_margin: egui::Margin::symmetric(8, 0),
            outer_margin: egui::Margin::ZERO,
            corner_radius: CornerRadius::same(18), // More rounded corners
            shadow: Default::default(),
            fill: if ui.visuals().dark_mode {
                Color32::from_rgb(30, 30, 30)
            } else {
                Color32::from_rgb(240, 240, 240)
            },
            stroke: if ui.visuals().dark_mode {
                Stroke::new(1.0, Color32::from_rgb(60, 60, 60))
            } else {
                Stroke::new(1.0, Color32::from_rgb(200, 200, 200))
            },
        };

        search_container
            .show(ui, |ui| {
                // Use layout to align items vertically centered
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);

                    let search_height = 34.0;
                    // Magnifying glass icon
                    ui.add(search_icon(16.0, search_height));

                    let before_len = input.len();

                    // Search input field
                    //let font_size = notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
                    let response = ui.add_sized(
                        [ui.available_width(), search_height],
                        TextEdit::singleline(input)
                            .hint_text(
                                RichText::new(tr!(
                                    i18n,
                                    "Search",
                                    "Placeholder for search input field"
                                ))
                                .weak(),
                            )
                            //.desired_width(available_width - 32.0)
                            //.font(egui::FontId::new(font_size, egui::FontFamily::Proportional))
                            .margin(vec2(0.0, 8.0))
                            .frame(false),
                    );

                    if response.has_focus() {
                        if ui.input(|i| i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowDown)) {
                            response.surrender_focus();
                        }
                    }

                    input_context(ui, &response, clipboard, input, PasteBehavior::Append);

                    let mut requested_focus = false;
                    if focus_state == FocusState::ShouldRequestFocus {
                        response.request_focus();
                        requested_focus = true;
                    }

                    let after_len = input.len();

                    let input_changed = before_len != after_len;

                    SearchResponse {
                        requested_focus,
                        input_changed,
                    }
                })
                .inner
            })
            .inner
    })
    .inner
}

#[derive(Debug, Eq, PartialEq)]
pub enum SearchType {
    String,
    NoteId(NoteId),
    Profile(Pubkey),
    Hashtag(String),
}

impl SearchType {
    fn get_type(query: &str) -> Self {
        if query.len() == 63 && query.starts_with("note1") {
            if let Some(noteid) = NoteId::from_bech(query) {
                return SearchType::NoteId(noteid);
            }
        } else if query.len() == 63 && query.starts_with("npub1") {
            if let Ok(pk) = Pubkey::try_from_bech32_string(query, false) {
                return SearchType::Profile(pk);
            }
        } else if query.chars().nth(0).is_some_and(|c| c == '#') {
            if let Some(hashtag) = query.get(1..) {
                return SearchType::Hashtag(hashtag.to_string());
            }
        }

        SearchType::String
    }

    fn search(
        &self,
        raw_query: &String,
        ndb: &Ndb,
        txn: &Transaction,
        max_results: u64,
    ) -> Option<Vec<NoteRef>> {
        match self {
            SearchType::String => search_string(raw_query, ndb, txn, max_results),
            SearchType::NoteId(noteid) => search_note(noteid, ndb, txn).map(|n| vec![n]),
            SearchType::Profile(pk) => search_pk(pk, ndb, txn, max_results),
            SearchType::Hashtag(hashtag) => search_hashtag(hashtag, ndb, txn, max_results),
        }
    }
}

fn search_string(
    query: &String,
    ndb: &Ndb,
    txn: &Transaction,
    max_results: u64,
) -> Option<Vec<NoteRef>> {
    let filter = Filter::new()
        .search(query)
        .kinds([1])
        .limit(max_results)
        .build();

    // TODO: execute in thread

    let before = Instant::now();
    let qrs = ndb.query(txn, &[filter], max_results as i32);
    let after = Instant::now();
    let duration = after - before;

    if duration > Duration::from_millis(20) {
        warn!(
            "query took {:?}... let's update this to use a thread!",
            after - before
        );
    }

    match qrs {
        Ok(qrs) => {
            info!("queried '{}' and got {} results", query, qrs.len());

            return Some(qrs.into_iter().map(NoteRef::from_query_result).collect());
        }

        Err(err) => {
            error!("fulltext query failed: {err}")
        }
    }

    None
}

fn search_note(noteid: &NoteId, ndb: &Ndb, txn: &Transaction) -> Option<NoteRef> {
    ndb.get_note_by_id(txn, noteid.bytes())
        .ok()
        .map(|n| NoteRef::from_note(&n))
}

fn search_pk(pk: &Pubkey, ndb: &Ndb, txn: &Transaction, max_results: u64) -> Option<Vec<NoteRef>> {
    let filter = Filter::new()
        .authors([pk.bytes()])
        .kinds([1])
        .limit(max_results)
        .build();

    let qrs = ndb.query(txn, &[filter], max_results as i32).ok()?;
    Some(qrs.into_iter().map(NoteRef::from_query_result).collect())
}

fn search_hashtag(
    hashtag_name: &str,
    ndb: &Ndb,
    txn: &Transaction,
    max_results: u64,
) -> Option<Vec<NoteRef>> {
    let filter = Filter::new()
        .kinds([1])
        .limit(max_results)
        .tags([hashtag_name], 't')
        .build();

    let qrs = ndb.query(txn, &[filter], max_results as i32).ok()?;
    Some(qrs.into_iter().map(NoteRef::from_query_result).collect())
}

fn recent_profile_item<'a>(
    profile: Option<&'a ProfileRecord<'_>>,
    pubkey: &'a Pubkey,
    _query: &'a str,
    is_selected: bool,
    width: f32,
    cache: &'a mut Images,
    accounts: &'a notedeck::Accounts,
) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        let min_img_size = 48.0;
        let spacing = 8.0;
        let body_font_size = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
        let x_button_size = 32.0;

        let (rect, resp) = ui.allocate_exact_size(
            vec2(width, min_img_size + 8.0),
            egui::Sense::click()
        );

        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

        if is_selected {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill,
            );
        }

        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().widgets.hovered.bg_fill,
            );
        }

        let pfp_rect = egui::Rect::from_min_size(
            rect.min + vec2(4.0, 4.0),
            vec2(min_img_size, min_img_size)
        );

        ui.put(
            pfp_rect,
            &mut ProfilePic::new(cache, get_profile_url(profile))
                .size(min_img_size)
                .with_follow_check(pubkey, accounts),
        );

        let name = get_display_name(profile).name();
        let name_font = egui::FontId::new(body_font_size, NotedeckTextStyle::Body.font_family());
        let painter = ui.painter();
        let text_galley = painter.layout(
            name.to_string(),
            name_font,
            ui.visuals().text_color(),
            width - min_img_size - spacing - x_button_size - 8.0,
        );

        let galley_pos = egui::Pos2::new(
            pfp_rect.right() + spacing,
            rect.center().y - (text_galley.rect.height() / 2.0)
        );

        painter.galley(galley_pos, text_galley, ui.visuals().text_color());

        let x_rect = egui::Rect::from_min_size(
            egui::Pos2::new(rect.right() - x_button_size, rect.top()),
            vec2(x_button_size, rect.height())
        );

        let x_center = x_rect.center();
        let x_size = 12.0;
        painter.line_segment(
            [
                egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y - x_size / 2.0),
                egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y + x_size / 2.0),
            ],
            egui::Stroke::new(1.5, ui.visuals().text_color()),
        );
        painter.line_segment(
            [
                egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y - x_size / 2.0),
                egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y + x_size / 2.0),
            ],
            egui::Stroke::new(1.5, ui.visuals().text_color()),
        );

        resp
    }
}

fn recent_search_item(query: &str, is_selected: bool, width: f32, _is_profile: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let min_img_size = 48.0;
        let spacing = 8.0;
        let body_font_size = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
        let x_button_size = 32.0;

        let (rect, resp) = ui.allocate_exact_size(
            vec2(width, min_img_size + 8.0),
            egui::Sense::click()
        );

        if is_selected {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill,
            );
        }

        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().widgets.hovered.bg_fill,
            );
        }

        let icon_rect = egui::Rect::from_min_size(
            rect.min + vec2(4.0, 4.0),
            vec2(min_img_size, min_img_size)
        );

        ui.put(icon_rect, search_icon(min_img_size / 2.0, min_img_size));

        let name_font = egui::FontId::new(body_font_size, NotedeckTextStyle::Body.font_family());
        let painter = ui.painter();
        let text_galley = painter.layout(
            query.to_string(),
            name_font,
            ui.visuals().text_color(),
            width - min_img_size - spacing - x_button_size - 8.0,
        );

        let galley_pos = egui::Pos2::new(
            icon_rect.right() + spacing,
            rect.center().y - (text_galley.rect.height() / 2.0)
        );

        painter.galley(galley_pos, text_galley, ui.visuals().text_color());

        let x_rect = egui::Rect::from_min_size(
            egui::Pos2::new(rect.right() - x_button_size, rect.top()),
            vec2(x_button_size, rect.height())
        );

        let x_center = x_rect.center();
        let x_size = 12.0;
        painter.line_segment(
            [
                egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y - x_size / 2.0),
                egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y + x_size / 2.0),
            ],
            egui::Stroke::new(1.5, ui.visuals().text_color()),
        );
        painter.line_segment(
            [
                egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y - x_size / 2.0),
                egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y + x_size / 2.0),
            ],
            egui::Stroke::new(1.5, ui.visuals().text_color()),
        );

        resp
    }
}

fn search_posts_button(query: &str, is_selected: bool, width: f32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let min_img_size = 48.0;
        let spacing = 8.0;
        let body_font_size = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);

        let (rect, resp) = ui.allocate_exact_size(
            vec2(width, min_img_size + 8.0),
            egui::Sense::click()
        );

        if is_selected {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill,
            );
        }

        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().widgets.hovered.bg_fill,
            );
        }

        let icon_rect = egui::Rect::from_min_size(
            rect.min + vec2(4.0, 4.0),
            vec2(min_img_size, min_img_size)
        );

        ui.put(icon_rect, search_icon(min_img_size / 2.0, min_img_size));

        let text = format!("Search posts for \"{}\"", query);
        let name_font = egui::FontId::new(body_font_size, NotedeckTextStyle::Body.font_family());
        let painter = ui.painter();
        let text_galley = painter.layout(
            text,
            name_font,
            ui.visuals().text_color(),
            width - min_img_size - spacing - 8.0,
        );

        let galley_pos = egui::Pos2::new(
            icon_rect.right() + spacing,
            rect.center().y - (text_galley.rect.height() / 2.0)
        );

        painter.galley(galley_pos, text_galley, ui.visuals().text_color());

        resp
    }
}

