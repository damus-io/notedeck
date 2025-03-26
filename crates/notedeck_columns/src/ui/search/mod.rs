use egui::{vec2, Align, Color32, CornerRadius, RichText, Stroke, TextEdit};
use enostr::KeypairUnowned;

use super::{note::contents::NoteContext, padding};
use crate::{
    actionbar::NoteAction,
    ui::{note::NoteOptions, timeline::TimelineTabView},
};
use egui_winit::clipboard::Clipboard;
use nostrdb::{Filter, Transaction};
use notedeck::{MuteFun, NoteRef};
use notedeck_ui::icons::search_icon;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

mod state;

pub use state::{FocusState, SearchQueryState, SearchState};

pub struct SearchView<'a, 'd> {
    query: &'a mut SearchQueryState,
    note_options: NoteOptions,
    txn: &'a Transaction,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
}

impl<'a, 'd> SearchView<'a, 'd> {
    pub fn new(
        txn: &'a Transaction,
        is_muted: &'a MuteFun,
        note_options: NoteOptions,
        query: &'a mut SearchQueryState,
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
    ) -> Self {
        Self {
            txn,
            is_muted,
            query,
            note_options,
            note_context,
            cur_acc,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, clipboard: &mut Clipboard) -> Option<NoteAction> {
        padding(8.0, ui, |ui| self.show_impl(ui, clipboard)).inner
    }

    pub fn show_impl(
        &mut self,
        ui: &mut egui::Ui,
        clipboard: &mut Clipboard,
    ) -> Option<NoteAction> {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 12.0);

        if search_box(self.query, ui, clipboard) {
            self.execute_search(ui.ctx());
        }

        match self.query.state {
            SearchState::New | SearchState::Navigating => None,

            SearchState::Searched | SearchState::Typing => {
                if self.query.state == SearchState::Typing {
                    ui.label(format!("Searching for '{}'", &self.query.string));
                } else {
                    ui.label(format!(
                        "Got {} results for '{}'",
                        self.query.notes.notes.len(),
                        &self.query.string
                    ));
                }

                egui::ScrollArea::vertical()
                    .show(ui, |ui| {
                        let reversed = false;
                        TimelineTabView::new(
                            &self.query.notes,
                            reversed,
                            self.note_options,
                            self.txn,
                            self.is_muted,
                            self.note_context,
                            self.cur_acc,
                        )
                        .show(ui)
                    })
                    .inner
            }
        }
    }

    fn execute_search(&mut self, ctx: &egui::Context) {
        if self.query.string.is_empty() {
            return;
        }

        let max_results = 500;
        let filter = Filter::new()
            .search(&self.query.string)
            .kinds([1])
            .limit(max_results)
            .build();

        // TODO: execute in thread

        let before = Instant::now();
        let qrs = self
            .note_context
            .ndb
            .query(self.txn, &[filter], max_results as i32);
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
                info!(
                    "queried '{}' and got {} results",
                    self.query.string,
                    qrs.len()
                );

                let note_refs = qrs.into_iter().map(NoteRef::from_query_result).collect();
                self.query.notes.notes = note_refs;
                self.query.notes.list.borrow_mut().reset();
                ctx.request_repaint();
            }

            Err(err) => {
                error!("fulltext query failed: {err}")
            }
        }
    }
}

fn search_box(query: &mut SearchQueryState, ui: &mut egui::Ui, clipboard: &mut Clipboard) -> bool {
    ui.horizontal(|ui| {
        // Container for search input and icon
        let search_container = egui::Frame {
            inner_margin: egui::Margin::symmetric(8, 0),
            outer_margin: egui::Margin::ZERO,
            corner_radius: CornerRadius::same(18), // More rounded corners
            shadow: Default::default(),
            fill: Color32::from_rgb(30, 30, 30), // Darker background to match screenshot
            stroke: Stroke::new(1.0, Color32::from_rgb(60, 60, 60)),
        };

        search_container
            .show(ui, |ui| {
                // Use layout to align items vertically centered
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);

                    let search_height = 34.0;
                    // Magnifying glass icon
                    ui.add(search_icon(16.0, search_height));

                    let before_len = query.string.len();

                    // Search input field
                    //let font_size = notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
                    let response = ui.add_sized(
                        [ui.available_width(), search_height],
                        TextEdit::singleline(&mut query.string)
                            .hint_text(RichText::new("Search notes...").weak())
                            //.desired_width(available_width - 32.0)
                            //.font(egui::FontId::new(font_size, egui::FontFamily::Proportional))
                            .margin(vec2(0.0, 8.0))
                            .frame(false),
                    );

                    response.context_menu(|ui| {
                        if ui.button("paste").clicked() {
                            if let Some(text) = clipboard.get() {
                                query.string.clear();
                                query.string.push_str(&text);
                            }
                        }
                    });

                    if response.middle_clicked() {
                        if let Some(text) = clipboard.get() {
                            query.string.clear();
                            query.string.push_str(&text);
                        }
                    }

                    if query.focus_state == FocusState::ShouldRequestFocus {
                        response.request_focus();
                        query.focus_state = FocusState::RequestedFocus;
                    }

                    let after_len = query.string.len();

                    let changed = before_len != after_len;
                    if changed {
                        query.mark_updated();
                    }

                    // Execute search after debouncing
                    if query.should_search() {
                        query.mark_searched(SearchState::Searched);
                        true
                    } else {
                        false
                    }
                })
                .inner
            })
            .inner
    })
    .inner
}
