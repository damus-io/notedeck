//! Publication reader view for NKBIP-01 publications (kind 30040/30041)

use egui::{Area, Color32, Frame, Order, ScrollArea, Stroke, Vec2};
use enostr::RelayPool;
use nostrdb::{Ndb, Transaction};
use notedeck::nav::DragResponse;
use notedeck::{ContextSelection, Localization, NoteAction};
use notedeck_ui::note::NoteContextButton;

use crate::timeline::publication::{PublicationSection, Publications};
use crate::timeline::PublicationSelection;

/// Reader mode for publications
#[derive(Default, Clone, Copy, PartialEq)]
pub enum ReaderMode {
    /// Continuous scrolling through all sections
    #[default]
    Continuous,
    /// One section at a time with pagination
    Paginated,
}

/// Persistent state for the publication reader (stored in egui memory)
#[derive(Default, Clone)]
struct ReaderState {
    mode: ReaderMode,
    current_section: usize,
    toc_visible: bool,
}

/// A publication reader view that displays the index and content sections
pub struct PublicationView<'a> {
    selection: &'a PublicationSelection,
    ndb: &'a Ndb,
    pool: &'a mut RelayPool,
    publications: &'a mut Publications,
    i18n: &'a mut Localization,
    col: usize,
}

/// Response from rendering that may contain a note action
pub struct PublicationViewResponse {
    pub action: Option<NoteAction>,
}

impl<'a> PublicationView<'a> {
    pub fn new(
        selection: &'a PublicationSelection,
        ndb: &'a Ndb,
        pool: &'a mut RelayPool,
        publications: &'a mut Publications,
        i18n: &'a mut Localization,
        col: usize,
    ) -> Self {
        Self {
            selection,
            ndb,
            pool,
            publications,
            i18n,
            col,
        }
    }

    fn state_id(&self) -> egui::Id {
        egui::Id::new(("publication_reader_state", self.selection.index_id.bytes(), self.col))
    }

    fn scroll_id(&self) -> egui::Id {
        egui::Id::new(("publication_scroll", self.selection.index_id.bytes(), self.col))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<Option<NoteAction>> {
        let txn = Transaction::new(self.ndb).expect("txn");

        // Open/get the publication state
        let _node = self.publications.open(
            self.ndb,
            self.pool,
            &txn,
            &self.selection.index_id,
        );

        // Poll for any newly fetched sections
        self.publications.poll_section_notes(self.ndb, &txn, &self.selection.index_id);

        // Get or create reader state
        let state_id = self.state_id();
        let mut state: ReaderState = ui.ctx().data_mut(|d| d.get_temp(state_id).unwrap_or_default());

        // Track any action from section context buttons
        let mut note_action: Option<NoteAction> = None;

        // Main layout
        let resp = ui.vertical(|ui| {
            // Render header bar
            self.render_header(ui, &txn, &mut state);

            ui.separator();

            // Content area
            let scroll_area = ScrollArea::vertical()
                .id_salt(self.scroll_id())
                .auto_shrink([false, false]);

            scroll_area.show(ui, |ui| {
                note_action = self.render_content(ui, &txn, &mut state);
            });
        });

        // Render TOC overlay if visible
        if state.toc_visible {
            self.render_toc_overlay(ui, &txn, &mut state);
        }

        // Save state
        ui.ctx().data_mut(|d| d.insert_temp(state_id, state));

        DragResponse::output(Some(note_action)).scroll_raw(resp.response.id)
    }

    fn render_header(&self, ui: &mut egui::Ui, txn: &Transaction, state: &mut ReaderState) {
        let node = self.publications.get(&self.selection.index_id);
        let section_count = node.map(|n| n.sections.len()).unwrap_or(0);

        // Get title from index note
        let title = self.ndb
            .get_note_by_id(txn, self.selection.index_id.bytes())
            .ok()
            .and_then(|note| self.get_tag_value(&note, "title").map(String::from))
            .unwrap_or_else(|| "Publication".to_string());

        ui.horizontal(|ui| {
            // TOC toggle button
            let toc_btn = if state.toc_visible { "✕ TOC" } else { "☰ TOC" };
            if ui.button(toc_btn).clicked() {
                state.toc_visible = !state.toc_visible;
            }

            ui.separator();

            // Mode toggle
            match state.mode {
                ReaderMode::Continuous => {
                    if ui.button("📖").on_hover_text("Switch to paginated view").clicked() {
                        state.mode = ReaderMode::Paginated;
                    }
                }
                ReaderMode::Paginated => {
                    if ui.button("📜").on_hover_text("Switch to continuous view").clicked() {
                        state.mode = ReaderMode::Continuous;
                    }

                    ui.separator();

                    // Navigation for paginated mode
                    if ui.add_enabled(state.current_section > 0, egui::Button::new("◀")).clicked() {
                        state.current_section = state.current_section.saturating_sub(1);
                    }

                    ui.label(format!("{}/{}", state.current_section + 1, section_count));

                    if ui.add_enabled(
                        state.current_section + 1 < section_count,
                        egui::Button::new("▶")
                    ).clicked() {
                        state.current_section += 1;
                    }
                }
            }

            ui.separator();

            // Title (truncated)
            let available = ui.available_width() - 20.0;
            ui.add(egui::Label::new(
                egui::RichText::new(&title).strong()
            ).truncate());
            let _ = available; // silence unused warning
        });
    }

    fn render_content(&mut self, ui: &mut egui::Ui, txn: &Transaction, state: &mut ReaderState) -> Option<NoteAction> {
        // Try to load the publication index note
        match self.ndb.get_note_by_id(txn, self.selection.index_id.bytes()) {
            Ok(note) => {
                self.render_publication(ui, txn, &note, state)
            }
            Err(_) => {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading("Loading Publication...");
                    ui.add_space(20.0);
                    ui.spinner();
                });
                None
            }
        }
    }

    fn render_publication(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        note: &nostrdb::Note,
        state: &mut ReaderState,
    ) -> Option<NoteAction> {
        let mut action = None;

        ui.vertical(|ui| {
            ui.add_space(16.0);

            // Publication info card
            let title = self.get_tag_value(note, "title").unwrap_or("Untitled Publication");
            let summary = self.get_tag_value(note, "summary");
            let author = self.get_author_name(txn, note);

            let info_frame = Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(12.0)
                .corner_radius(8.0);

            info_frame.show(ui, |ui| {
                ui.heading(title);

                if let Some(author) = author {
                    ui.add_space(4.0);
                    ui.label(format!("by {}", author));
                }

                if let Some(summary) = summary {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(summary).italics());
                }

                if let Some(node) = self.publications.get(&self.selection.index_id) {
                    ui.add_space(4.0);
                    let fetched = node.sections.iter().filter(|s| s.note_key.is_some()).count();
                    ui.label(egui::RichText::new(format!(
                        "Sections: {}/{}",
                        fetched,
                        node.sections.len()
                    )).small());
                }
            });

            ui.add_space(16.0);

            // Render sections based on mode
            // Clone sections to avoid borrow conflict with self
            let sections_opt = self.publications.get(&self.selection.index_id).map(|node| {
                (node.sections.clone(), node.fetch_complete)
            });

            if let Some((sections, fetch_complete)) = sections_opt {
                action = match state.mode {
                    ReaderMode::Continuous => self.render_continuous(ui, txn, &sections, fetch_complete),
                    ReaderMode::Paginated => self.render_paginated(ui, txn, &sections, state),
                };
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading sections...");
                });
            }
        });

        action
    }

    fn render_continuous(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        sections: &[PublicationSection],
        fetch_complete: bool,
    ) -> Option<NoteAction> {
        let mut action = None;

        if sections.is_empty() {
            ui.label("This publication has no content sections.");
            return None;
        }

        for section in sections.iter() {
            if let Some(a) = self.render_section_card(ui, txn, section) {
                action = Some(a);
            }
            ui.add_space(12.0);
        }

        // Fetch status at bottom
        if !fetch_complete {
            ui.horizontal(|ui| {
                ui.spinner();
                let fetched = sections.iter().filter(|s| s.note_key.is_some()).count();
                ui.label(format!("Fetching... ({}/{})", fetched, sections.len()));
            });
        }

        action
    }

    fn render_paginated(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        sections: &[PublicationSection],
        state: &mut ReaderState,
    ) -> Option<NoteAction> {
        let mut action = None;

        if sections.is_empty() {
            ui.label("This publication has no content sections.");
            return None;
        }

        // Clamp current section to valid range
        if state.current_section >= sections.len() {
            state.current_section = sections.len().saturating_sub(1);
        }

        let section = &sections[state.current_section];
        let section_title = section.title.as_deref().unwrap_or(&section.dtag);

        // Section header - title on left, options button on right
        let header_resp = ui.horizontal(|ui| {
            ui.heading(section_title);
        });

        // Add options button at right side of header (if we have the note)
        if let Some(note_key) = section.note_key {
            let context_pos = {
                let size = NoteContextButton::max_width();
                let header_rect = header_resp.response.rect;
                let min = egui::pos2(ui.max_rect().right() - size - 8.0, header_rect.top());
                egui::Rect::from_min_size(min, egui::vec2(size, size))
            };

            let options_resp = ui.add(NoteContextButton::new(note_key).place_at(context_pos));
            if let Some(ctx_action) = NoteContextButton::menu(ui, self.i18n, options_resp) {
                action = Some(NoteAction::Context(ContextSelection {
                    note_key,
                    action: ctx_action,
                }));
            }
        }

        ui.add_space(16.0);

        // Section content
        if let Some(note_key) = section.note_key {
            if let Ok(section_note) = self.ndb.get_note_by_key(txn, note_key) {
                let content = section_note.content();
                if !content.is_empty() {
                    Self::render_text_content(ui, content);
                } else {
                    ui.label(
                        egui::RichText::new("(empty section)")
                            .color(Color32::GRAY)
                            .italics()
                    );
                }
            } else {
                ui.label(
                    egui::RichText::new("Error loading section")
                        .color(Color32::RED)
                );
            }
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.spinner();
                ui.label("Loading section...");
            });
        }

        // Bottom navigation
        ui.add_space(24.0);
        ui.separator();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            let has_prev = state.current_section > 0;
            let has_next = state.current_section + 1 < sections.len();

            if ui.add_enabled(has_prev, egui::Button::new("← Previous Chapter")).clicked() {
                state.current_section = state.current_section.saturating_sub(1);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(has_next, egui::Button::new("Next Chapter →")).clicked() {
                    state.current_section += 1;
                }
            });
        });

        action
    }

    fn render_section_card(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        section: &PublicationSection,
    ) -> Option<NoteAction> {
        let mut action = None;
        let section_title = section.title.as_deref().unwrap_or(&section.dtag);

        // Card frame
        let card_frame = Frame::default()
            .stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .inner_margin(12.0)
            .corner_radius(6.0);

        let card_resp = card_frame.show(ui, |ui| {
            // Section header
            ui.heading(section_title);

            ui.add_space(8.0);

            // Section content
            if let Some(note_key) = section.note_key {
                if let Ok(section_note) = self.ndb.get_note_by_key(txn, note_key) {
                    let content = section_note.content();
                    if !content.is_empty() {
                        Self::render_text_content(ui, content);
                    } else {
                        ui.label(
                            egui::RichText::new("(empty section)")
                                .color(Color32::GRAY)
                                .italics()
                        );
                    }
                } else {
                    ui.label(
                        egui::RichText::new("Error loading section")
                            .color(Color32::RED)
                    );
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Loading...")
                            .color(Color32::GRAY)
                            .italics()
                    );
                });
            }
        });

        // Add options button at top-right of card (if we have the note)
        if let Some(note_key) = section.note_key {
            let context_pos = {
                let size = NoteContextButton::max_width();
                let top_right = card_resp.response.rect.right_top();
                let min = egui::pos2(top_right.x - size - 12.0, top_right.y + 12.0); // offset for padding
                egui::Rect::from_min_size(min, egui::vec2(size, size))
            };

            let options_resp = ui.add(NoteContextButton::new(note_key).place_at(context_pos));
            if let Some(ctx_action) = NoteContextButton::menu(ui, self.i18n, options_resp) {
                action = Some(NoteAction::Context(ContextSelection {
                    note_key,
                    action: ctx_action,
                }));
            }
        }

        action
    }

    /// Render text content with proper word wrapping and paragraph breaks
    fn render_text_content(ui: &mut egui::Ui, content: &str) {
        // Split into paragraphs (double newlines)
        let paragraphs: Vec<&str> = content.split("\n\n").collect();

        for (i, paragraph) in paragraphs.iter().enumerate() {
            if i > 0 {
                ui.add_space(12.0); // Space between paragraphs
            }

            // Trim and skip empty paragraphs
            let trimmed = paragraph.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Render with word wrapping enabled
            ui.add(egui::Label::new(trimmed).wrap());
        }
    }

    fn render_toc_overlay(&self, ui: &mut egui::Ui, _txn: &Transaction, state: &mut ReaderState) {
        let screen_rect = ui.ctx().screen_rect();

        // Dimmed background
        Area::new(egui::Id::new(("pub_toc_bg", self.selection.index_id.bytes())))
            .order(Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ui.ctx(), |ui| {
                let response = ui.allocate_response(screen_rect.size(), egui::Sense::click());
                ui.painter().rect_filled(
                    screen_rect,
                    0.0,
                    Color32::from_black_alpha(128),
                );
                // Click outside to close
                if response.clicked() {
                    state.toc_visible = false;
                }
            });

        // TOC drawer
        let drawer_width = (screen_rect.width() * 0.7).min(300.0);
        let drawer_rect = egui::Rect::from_min_size(
            screen_rect.min,
            Vec2::new(drawer_width, screen_rect.height()),
        );

        Area::new(egui::Id::new(("pub_toc_drawer", self.selection.index_id.bytes())))
            .order(Order::Foreground)
            .fixed_pos(drawer_rect.min)
            .show(ui.ctx(), |ui| {
                Frame::default()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.set_min_size(drawer_rect.size());
                        ui.set_max_width(drawer_width);

                        // Header
                        ui.horizontal(|ui| {
                            ui.heading("Table of Contents");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("✕").clicked() {
                                    state.toc_visible = false;
                                }
                            });
                        });

                        ui.separator();
                        ui.add_space(8.0);

                        // TOC entries
                        ScrollArea::vertical()
                            .id_salt("toc_scroll")
                            .show(ui, |ui| {
                                if let Some(node) = self.publications.get(&self.selection.index_id) {
                                    for (i, section) in node.sections.iter().enumerate() {
                                        let title = section.title.as_deref().unwrap_or(&section.dtag);
                                        let is_current = state.mode == ReaderMode::Paginated
                                            && state.current_section == i;

                                        let status = if section.note_key.is_some() {
                                            ""
                                        } else {
                                            " ⏳"
                                        };

                                        let text = format!("{}{}", title, status);
                                        let rich_text = if is_current {
                                            egui::RichText::new(text).strong().color(ui.visuals().selection.stroke.color)
                                        } else {
                                            egui::RichText::new(text)
                                        };

                                        if ui.add(egui::Label::new(rich_text).sense(egui::Sense::click())).clicked() {
                                            state.current_section = i;
                                            state.mode = ReaderMode::Paginated;
                                            state.toc_visible = false;
                                        }

                                        ui.add_space(4.0);
                                    }
                                }
                            });
                    });
            });
    }

    fn get_author_name(&self, txn: &Transaction, note: &nostrdb::Note) -> Option<String> {
        let pubkey = note.pubkey();
        self.ndb
            .get_profile_by_pubkey(txn, pubkey)
            .ok()
            .map(|p| {
                let name = notedeck::name::get_display_name(Some(&p));
                let s = name.name();
                if s == "??" { None } else { Some(s.to_string()) }
            })
            .flatten()
    }

    /// Get a tag value by tag name (e.g., "title", "summary")
    fn get_tag_value<'t>(&self, note: &'t nostrdb::Note, tag_name: &str) -> Option<&'t str> {
        let tags = note.tags();
        for tag in tags {
            if tag.count() >= 2 {
                if let Some(name) = tag.get(0).and_then(|t| t.variant().str()) {
                    if name == tag_name {
                        return tag.get(1).and_then(|t| t.variant().str());
                    }
                }
            }
        }
        None
    }
}
