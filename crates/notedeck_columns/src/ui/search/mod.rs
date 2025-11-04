use egui::{vec2, Align, Color32, CornerRadius, RichText, Stroke, TextEdit};
use enostr::{NoteId, Pubkey};
use state::TypingType;

use crate::{
    nav::BodyResponse,
    timeline::{TimelineTab, TimelineUnits},
    ui::timeline::TimelineTabView,
};
use egui_winit::clipboard::Clipboard;
use nostrdb::{Filter, Ndb, Transaction};
use notedeck::{tr, tr_plural, JobsCache, Localization, NoteAction, NoteContext, NoteRef};

use notedeck_ui::{
    context_menu::{input_context, PasteBehavior},
    icons::search_icon,
    padding, NoteOptions,
};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

mod state;

pub use state::{FocusState, SearchQueryState, SearchState};

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
        padding(8.0, ui, |ui| self.show_impl(ui)).inner
    }

    pub fn show_impl(&mut self, ui: &mut egui::Ui) -> BodyResponse<NoteAction> {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 12.0);

        let search_resp = search_box(
            self.note_context.i18n,
            &mut self.query.string,
            self.query.focus_state.clone(),
            ui,
            self.note_context.clipboard,
        );

        search_resp.process(self.query);

        let mut search_action = None;
        let mut body_resp = BodyResponse::none();
        match &self.query.state {
            SearchState::New | SearchState::Navigating => {}
            SearchState::Typing(TypingType::Mention(mention_name)) => 's: {
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
                )
                .show_in_rect(ui.available_rect_before_wrap(), ui);

                let Some(res) = search_res.output else {
                    break 's;
                };

                search_action = match res {
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
            SearchState::PerformSearch(search_type) => {
                execute_search(
                    ui.ctx(),
                    search_type,
                    &self.query.string,
                    self.note_context,
                    self.txn,
                    &mut self.query.notes,
                );
                search_action = Some(SearchAction::Searched);
                body_resp.insert(self.show_search_results(ui));
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
                body_resp.insert(self.show_search_results(ui));
            }
            SearchState::Typing(TypingType::AutoSearch) => {
                ui.label(tr!(
                    self.note_context.i18n,
                    "Searching for '{query}'",
                    "Search in progress message",
                    query = &self.query.string
                ));

                body_resp.insert(self.show_search_results(ui));
            }
        };

        if let Some(resp) = search_action {
            resp.process(self.query);
        }

        body_resp
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
    note_context: &mut NoteContext,
    txn: &Transaction,
    tab: &mut TimelineTab,
) {
    if raw_input.is_empty() {
        return;
    }

    let max_results = 500;
    let ndb = note_context.ndb;

    let Some(note_refs) = search_type.search(raw_input, ndb, txn, max_results) else {
        handle_search_miss(ctx, search_type, note_context, txn);
        return;
    };

    tab.units = TimelineUnits::from_refs_single(note_refs);
    tab.list.borrow_mut().reset();
    ctx.request_repaint();
}

fn handle_search_miss(
    ctx: &egui::Context,
    search_type: &SearchType,
    note_context: &mut NoteContext,
    txn: &Transaction,
) {
    match search_type {
        SearchType::NoteId(note_id) => {
            // Queue the missing event so the shared outbox worker can fetch it
            // from the author's relays if we do not already have it locally.
            note_context
                .unknown_ids
                .add_note_id_if_missing(note_context.ndb, txn, note_id.bytes());
            note_context.drive_unknown_ids(ctx);
        }
        SearchType::Profile(pubkey) => {
            // Trigger a background fetch for the profile timeline so the view
            // can hydrate once data arrives from fallback relays.
            note_context
                .unknown_ids
                .add_pubkey_if_missing(note_context.ndb, txn, pubkey.bytes());
            note_context.drive_unknown_ids(ctx);
        }
        SearchType::String | SearchType::Hashtag(_) => {
            // Plain-text and hashtag searches rely entirely on local
            // full-text indices today; once remote federation is available we
            // can route through outbox-aware search providers here.
        }
    }
}

enum SearchAction {
    NewSearch {
        search_type: SearchType,
        new_search_text: String,
    },
    Searched,
    CloseMention,
}

impl SearchAction {
    fn process(self, state: &mut SearchQueryState) {
        match self {
            SearchAction::NewSearch {
                search_type,
                new_search_text,
            } => {
                state.state = SearchState::PerformSearch(search_type);
                state.string = new_search_text;
            }
            SearchAction::CloseMention => state.state = SearchState::New,
            SearchAction::Searched => state.state = SearchState::Searched,
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
        }

        if state.string.chars().nth(0) != Some('@') {
            if self.input_changed {
                state.state = SearchState::Typing(TypingType::AutoSearch);
                state.debouncer.bounce();
            }

            if state.state == SearchState::Typing(TypingType::AutoSearch)
                && state.debouncer.should_act()
            {
                state.state = SearchState::PerformSearch(SearchType::get_type(&state.string));
            }

            return;
        }

        if self.input_changed {
            if let Some(mention_text) = state.string.get(1..) {
                state.state = SearchState::Typing(TypingType::Mention(mention_text.to_owned()));
            }
        }
    }
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
                                    "Search notes...",
                                    "Placeholder for search notes input field"
                                ))
                                .weak(),
                            )
                            //.desired_width(available_width - 32.0)
                            //.font(egui::FontId::new(font_size, egui::FontFamily::Proportional))
                            .margin(vec2(0.0, 8.0))
                            .frame(false),
                    );

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
