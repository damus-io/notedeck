//! Publication reader view for NKBIP-01 publications (kind 30040/30041)
//!
//! Uses tree-based navigation for hierarchical publications.

use egui::{Area, Color32, Frame, Order, ScrollArea, Stroke, Vec2};
use enostr::{NoteId, RelayPool};
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::nav::DragResponse;
use notedeck::{ContextSelection, Localization, NoteAction};
use notedeck_ui::note::NoteContextButton;
use std::collections::HashSet;

use crate::timeline::publication::{Publications, PublicationTreeState};
use crate::timeline::PublicationSelection;

/// Navigation actions for publication reader
#[derive(Debug, Clone)]
pub enum PublicationNavAction {
    /// Navigate back to previous publication in history
    Back,
    /// Navigate into a nested publication
    NavigateInto(NoteId),
    /// Index view: drill down into a child node
    DrillDown(usize),
    /// Index view: go up one level to parent
    DrillUp,
    /// Index view: navigate to previous sibling
    PrevSibling,
    /// Index view: navigate to next sibling
    NextSibling,
}

/// Lightweight section data for rendering (avoids borrow conflicts)
#[derive(Clone)]
struct SectionData {
    #[allow(dead_code)]
    index: usize,
    title: String,
    note_key: Option<NoteKey>,
}

/// Reader mode for publications
#[derive(Default, Clone, Copy, PartialEq)]
pub enum ReaderMode {
    /// Continuous scrolling through all sections
    #[default]
    Continuous,
    /// One section at a time with pagination
    Paginated,
    /// Index view - shows children of current node for drill-down navigation
    Index,
}

/// State for index view navigation within the tree
#[derive(Clone, Default)]
struct IndexViewState {
    /// Current node index being viewed (0 = root)
    current_node: usize,
}

/// Persistent state for the publication reader (stored in egui memory)
#[derive(Default, Clone)]
struct ReaderState {
    mode: ReaderMode,
    /// Current leaf index in the tree (for paginated mode)
    current_leaf_index: usize,
    toc_visible: bool,
    /// Expanded branch nodes in the TOC
    expanded_branches: HashSet<usize>,
    /// Index view state (current position in tree for drill-down navigation)
    index_view: IndexViewState,
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

/// Response from rendering that may contain actions
pub struct PublicationViewResponse {
    pub action: Option<NoteAction>,
    pub nav_action: Option<PublicationNavAction>,
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
        egui::Id::new((
            "publication_reader_state",
            self.selection.index_id.bytes(),
            self.col,
        ))
    }

    fn scroll_id(&self) -> egui::Id {
        egui::Id::new((
            "publication_scroll",
            self.selection.index_id.bytes(),
            self.col,
        ))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<PublicationViewResponse> {
        let txn = Transaction::new(self.ndb).expect("txn");

        // Open/get the publication state
        let _state = self.publications.open(
            self.ndb,
            self.pool,
            &txn,
            &self.selection.index_id,
        );

        // Poll for any newly fetched sections
        self.publications.poll_updates(
            self.ndb,
            self.pool,
            &txn,
            &self.selection.index_id,
        );

        // Get or create reader state
        let state_id = self.state_id();
        let mut state: ReaderState = ui.ctx().data_mut(|d| {
            d.get_temp(state_id).unwrap_or_else(|| {
                // New state - check for default mode preference from Publications feed
                let default_mode_id = crate::ui::timeline::publication_default_mode_id(self.col);
                let default_mode: ReaderMode = d.get_temp(default_mode_id).unwrap_or_default();
                ReaderState {
                    mode: default_mode,
                    ..Default::default()
                }
            })
        });

        // Track actions
        let mut note_action: Option<NoteAction> = None;
        let mut nav_action: Option<PublicationNavAction> = None;

        // Main layout
        let resp = ui.vertical(|ui| {
            // Render header bar (may return navigation action)
            nav_action = self.render_header(ui, &txn, &mut state);

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

        let response = PublicationViewResponse {
            action: note_action,
            nav_action,
        };

        DragResponse::output(Some(response)).scroll_raw(resp.response.id)
    }

    fn render_header(
        &self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        state: &mut ReaderState,
    ) -> Option<PublicationNavAction> {
        let pub_state = self.publications.get(&self.selection.index_id);
        let section_count = pub_state.map(|s| s.section_count()).unwrap_or(0);

        // Get title from root node
        let title = pub_state
            .map(|s| s.root().display_title().to_string())
            .unwrap_or_else(|| "Publication".to_string());

        let mut nav_action = None;

        ui.horizontal(|ui| {
            // Back button (only shown when we have history)
            if self.selection.can_go_back() {
                if ui
                    .button("←")
                    .on_hover_text("Back to previous publication")
                    .clicked()
                {
                    nav_action = Some(PublicationNavAction::Back);
                }
                ui.separator();
            }

            // TOC toggle button
            let toc_btn = if state.toc_visible { "✕ TOC" } else { "☰ TOC" };
            if ui.button(toc_btn).clicked() {
                state.toc_visible = !state.toc_visible;
            }

            ui.separator();

            // Mode toggle
            match state.mode {
                ReaderMode::Continuous => {
                    if ui
                        .button("📖")
                        .on_hover_text("Switch to paginated view")
                        .clicked()
                    {
                        state.mode = ReaderMode::Paginated;
                    }
                    if ui
                        .button("📑")
                        .on_hover_text("Switch to index view")
                        .clicked()
                    {
                        state.mode = ReaderMode::Index;
                        state.index_view = IndexViewState::default();
                    }
                }
                ReaderMode::Paginated => {
                    if ui
                        .button("📜")
                        .on_hover_text("Switch to continuous view")
                        .clicked()
                    {
                        state.mode = ReaderMode::Continuous;
                    }
                    if ui
                        .button("📑")
                        .on_hover_text("Switch to index view")
                        .clicked()
                    {
                        state.mode = ReaderMode::Index;
                        state.index_view = IndexViewState::default();
                    }

                    ui.separator();

                    // Navigation for paginated mode
                    if ui
                        .add_enabled(state.current_leaf_index > 0, egui::Button::new("◀"))
                        .clicked()
                    {
                        state.current_leaf_index = state.current_leaf_index.saturating_sub(1);
                    }

                    ui.label(format!(
                        "{}/{}",
                        state.current_leaf_index + 1,
                        section_count
                    ));

                    if ui
                        .add_enabled(
                            state.current_leaf_index + 1 < section_count,
                            egui::Button::new("▶"),
                        )
                        .clicked()
                    {
                        state.current_leaf_index += 1;
                    }
                }
                ReaderMode::Index => {
                    // Mode toggles
                    if ui
                        .button("📜")
                        .on_hover_text("Switch to continuous view")
                        .clicked()
                    {
                        state.mode = ReaderMode::Continuous;
                        state.index_view = IndexViewState::default();
                    }

                    ui.separator();

                    // Index navigation controls
                    let current_node = state.index_view.current_node;

                    // Up button (when not at root)
                    if current_node != 0 {
                        if let Some(ps) = pub_state {
                            if let Some(node) = ps.get_node(current_node) {
                                if let Some(parent_idx) = node.parent {
                                    if ui
                                        .button("⬆")
                                        .on_hover_text("Go up to parent")
                                        .clicked()
                                    {
                                        state.index_view.current_node = parent_idx;
                                    }
                                }
                            }
                        }
                    }

                    // Sibling navigation (Prev/Next)
                    if let Some(ps) = pub_state {
                        let (prev_sibling, next_sibling) = ps.tree.siblings(current_node);

                        if ui
                            .add_enabled(prev_sibling.is_some(), egui::Button::new("◀"))
                            .on_hover_text("Previous sibling")
                            .clicked()
                        {
                            if let Some(prev_idx) = prev_sibling {
                                state.index_view.current_node = prev_idx;
                            }
                        }

                        if ui
                            .add_enabled(next_sibling.is_some(), egui::Button::new("▶"))
                            .on_hover_text("Next sibling")
                            .clicked()
                        {
                            if let Some(next_idx) = next_sibling {
                                state.index_view.current_node = next_idx;
                            }
                        }
                    }
                }
            }

            ui.separator();

            // Breadcrumbs (if we have history)
            if !self.selection.breadcrumbs().is_empty() {
                self.render_breadcrumbs(ui, txn);
                ui.label("›");
            }

            // Current title (truncated)
            ui.add(egui::Label::new(egui::RichText::new(&title).strong()).truncate());
        });

        nav_action
    }

    fn render_breadcrumbs(&self, ui: &mut egui::Ui, txn: &Transaction) {
        let breadcrumbs = self.selection.breadcrumbs();

        for (i, note_id) in breadcrumbs.iter().enumerate() {
            // Get title for this breadcrumb
            let crumb_title = self
                .ndb
                .get_note_by_id(txn, note_id.bytes())
                .ok()
                .and_then(|note| self.get_tag_value(&note, "title").map(|s| s.to_string()))
                .unwrap_or_else(|| "...".to_string());

            // Truncate long titles
            let display_title = if crumb_title.len() > 15 {
                format!("{}…", &crumb_title[..12])
            } else {
                crumb_title
            };

            ui.label(
                egui::RichText::new(&display_title)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );

            if i < breadcrumbs.len() - 1 {
                ui.label(egui::RichText::new("›").small());
            }
        }
    }

    fn render_content(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        state: &mut ReaderState,
    ) -> Option<NoteAction> {
        // Try to load the publication index note
        match self.ndb.get_note_by_id(txn, self.selection.index_id.bytes()) {
            Ok(note) => self.render_publication(ui, txn, &note, state),
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
            let title = self
                .get_tag_value(note, "title")
                .unwrap_or("Untitled Publication");
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

                if let Some(pub_state) = self.publications.get(&self.selection.index_id) {
                    ui.add_space(4.0);
                    let resolved = pub_state.resolved_section_count();
                    let total = pub_state.section_count();
                    ui.label(
                        egui::RichText::new(format!("Sections: {}/{}", resolved, total)).small(),
                    );
                }
            });

            ui.add_space(16.0);

            // Get section data before rendering (to avoid borrow conflicts)
            let section_data: Option<(Vec<SectionData>, bool)> =
                self.publications.get(&self.selection.index_id).map(|pub_state| {
                    let is_complete = pub_state.is_complete();
                    let sections: Vec<SectionData> = pub_state
                        .resolved_sections()
                        .map(|(idx, node)| SectionData {
                            index: idx,
                            title: node.display_title().to_string(),
                            note_key: node.note_key,
                        })
                        .collect();
                    (sections, is_complete)
                });

            // Render sections based on mode
            if let Some((sections, is_complete)) = section_data {
                action = match state.mode {
                    ReaderMode::Continuous => {
                        self.render_continuous(ui, txn, &sections, is_complete)
                    }
                    ReaderMode::Paginated => self.render_paginated(ui, txn, &sections, state),
                    ReaderMode::Index => self.render_index_view(ui, txn, state)
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
        sections: &[SectionData],
        is_complete: bool,
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
        if !is_complete {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Fetching... ({}/{})", sections.len(), sections.len()));
            });
        }

        action
    }

    fn render_paginated(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        sections: &[SectionData],
        state: &mut ReaderState,
    ) -> Option<NoteAction> {
        let mut action = None;

        if sections.is_empty() {
            ui.label("This publication has no content sections.");
            return None;
        }

        // Clamp current section to valid range
        if state.current_leaf_index >= sections.len() {
            state.current_leaf_index = sections.len().saturating_sub(1);
        }

        let section = &sections[state.current_leaf_index];
        let section_title = &section.title;

        // Section header
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
                            .italics(),
                    );
                }
            } else {
                ui.label(egui::RichText::new("Error loading section").color(Color32::RED));
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
            let has_prev = state.current_leaf_index > 0;
            let has_next = state.current_leaf_index + 1 < sections.len();

            if ui
                .add_enabled(has_prev, egui::Button::new("← Previous Chapter"))
                .clicked()
            {
                state.current_leaf_index = state.current_leaf_index.saturating_sub(1);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(has_next, egui::Button::new("Next Chapter →"))
                    .clicked()
                {
                    state.current_leaf_index += 1;
                }
            });
        });

        action
    }

    /// Render index view - shows immediate children of current node
    fn render_index_view(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        state: &mut ReaderState,
    ) -> Option<NoteAction> {
        let mut action = None;
        let current_node = state.index_view.current_node;

        let Some(pub_state) = self.publications.get(&self.selection.index_id) else {
            ui.label("Loading publication...");
            return None;
        };

        // Get current node info
        let Some(node) = pub_state.get_node(current_node) else {
            ui.label("Node not found");
            return None;
        };

        // Show current node title
        let title = node.display_title();
        ui.heading(title);
        ui.add_space(8.0);

        // Render breadcrumb path within the tree
        self.render_index_breadcrumbs(ui, pub_state, current_node);
        ui.add_space(16.0);

        // Get children of current node
        let children: Vec<_> = pub_state
            .children(current_node)
            .map(|children| {
                children
                    .into_iter()
                    .filter_map(|child| {
                        pub_state.tree.get_index(&child.address).map(|idx| {
                            (
                                idx,
                                child.display_title().to_string(),
                                child.is_branch(),
                                child.is_resolved(),
                                child.note_key,
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if children.is_empty() {
            // This is a leaf node or has no children - show content instead
            if let Some(note_key) = node.note_key {
                if let Ok(note) = self.ndb.get_note_by_key(txn, note_key) {
                    let content = note.content();
                    if !content.is_empty() {
                        Self::render_text_content(ui, content);
                    } else {
                        ui.label(
                            egui::RichText::new("(empty section)")
                                .color(Color32::GRAY)
                                .italics(),
                        );
                    }
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading content...");
                });
            }
            return action;
        }

        // Render each child as a card
        for (child_idx, child_title, is_branch, is_resolved, note_key) in children {
            if let Some(a) =
                self.render_index_child_card(ui, txn, state, child_idx, &child_title, is_branch, is_resolved, note_key)
            {
                action = Some(a);
            }
            ui.add_space(8.0);
        }

        action
    }

    /// Render breadcrumbs for index view (path within the tree)
    fn render_index_breadcrumbs(
        &self,
        ui: &mut egui::Ui,
        pub_state: &PublicationTreeState,
        current_node: usize,
    ) {
        let hierarchy = pub_state.tree.hierarchy(current_node);

        if hierarchy.len() <= 1 {
            // At root, no breadcrumbs needed
            return;
        }

        ui.horizontal(|ui| {
            for (i, &idx) in hierarchy.iter().enumerate() {
                if let Some(node) = pub_state.get_node(idx) {
                    let title = node.display_title();
                    let display = if title.len() > 20 {
                        format!("{}...", &title[..17])
                    } else {
                        title.to_string()
                    };

                    let is_current = i == hierarchy.len() - 1;
                    let text_color = if is_current {
                        ui.visuals().text_color()
                    } else {
                        ui.visuals().weak_text_color()
                    };

                    ui.label(egui::RichText::new(&display).small().color(text_color));

                    if !is_current {
                        ui.label(egui::RichText::new(" > ").small());
                    }
                }
            }
        });
    }

    /// Render a child card in index view
    fn render_index_child_card(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        state: &mut ReaderState,
        child_idx: usize,
        title: &str,
        is_branch: bool,
        is_resolved: bool,
        note_key: Option<NoteKey>,
    ) -> Option<NoteAction> {
        let mut action = None;

        let card_frame = Frame::default()
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .inner_margin(12.0)
            .corner_radius(6.0);

        let card_resp = card_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // Type indicator
                let icon = if is_branch { "📁" } else { "📄" };
                ui.label(icon);

                // Title
                let title_text = if !is_resolved {
                    format!("{} ⏳", title)
                } else {
                    title.to_string()
                };

                if is_branch {
                    // Branch: clickable to drill down
                    if ui
                        .add(
                            egui::Label::new(egui::RichText::new(&title_text).strong())
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Click to expand")
                        .clicked()
                    {
                        state.index_view.current_node = child_idx;
                    }
                } else {
                    // Leaf: show title
                    ui.label(egui::RichText::new(&title_text).strong());
                }
            });

            // For leaf nodes, show content
            if !is_branch {
                ui.add_space(8.0);

                if let Some(note_key) = note_key {
                    if let Ok(note) = self.ndb.get_note_by_key(txn, note_key) {
                        let content = note.content();
                        if !content.is_empty() {
                            Self::render_text_content(ui, content);
                        } else {
                            ui.label(
                                egui::RichText::new("(empty section)")
                                    .color(Color32::GRAY)
                                    .italics(),
                            );
                        }
                    } else {
                        ui.label(egui::RichText::new("Error loading content").color(Color32::RED));
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Loading...")
                                .color(Color32::GRAY)
                                .italics(),
                        );
                    });
                }
            }
        });

        // Add context button for leaf nodes with content
        if !is_branch {
            if let Some(note_key) = note_key {
                let context_pos = {
                    let size = NoteContextButton::max_width();
                    let top_right = card_resp.response.rect.right_top();
                    let min = egui::pos2(top_right.x - size - 12.0, top_right.y + 12.0);
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
        }

        action
    }

    fn render_section_card(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        section: &SectionData,
    ) -> Option<NoteAction> {
        let mut action = None;
        let section_title = &section.title;

        // Card frame
        let card_frame = Frame::default()
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
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
                                .italics(),
                        );
                    }
                } else {
                    ui.label(egui::RichText::new("Error loading section").color(Color32::RED));
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Loading...")
                            .color(Color32::GRAY)
                            .italics(),
                    );
                });
            }
        });

        // Add options button at top-right of card (if we have the note)
        if let Some(note_key) = section.note_key {
            let context_pos = {
                let size = NoteContextButton::max_width();
                let top_right = card_resp.response.rect.right_top();
                let min = egui::pos2(top_right.x - size - 12.0, top_right.y + 12.0);
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
                ui.add_space(12.0);
            }

            let trimmed = paragraph.trim();
            if trimmed.is_empty() {
                continue;
            }

            ui.add(egui::Label::new(trimmed).wrap());
        }
    }

    fn render_toc_overlay(&self, ui: &mut egui::Ui, _txn: &Transaction, state: &mut ReaderState) {
        let screen_rect = ui.ctx().screen_rect();

        // Dimmed background
        Area::new(egui::Id::new((
            "pub_toc_bg",
            self.selection.index_id.bytes(),
        )))
        .order(Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(ui.ctx(), |ui| {
            let response = ui.allocate_response(screen_rect.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(screen_rect, 0.0, Color32::from_black_alpha(128));
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

        Area::new(egui::Id::new((
            "pub_toc_drawer",
            self.selection.index_id.bytes(),
        )))
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
                    ScrollArea::vertical().id_salt("toc_scroll").show(ui, |ui| {
                        if let Some(pub_state) = self.publications.get(&self.selection.index_id) {
                            self.render_toc_tree(ui, pub_state, state, 0, 0);
                        }
                    });
                });
        });
    }

    /// Recursively render TOC tree with collapsible branches
    fn render_toc_tree(
        &self,
        ui: &mut egui::Ui,
        pub_state: &PublicationTreeState,
        state: &mut ReaderState,
        node_index: usize,
        depth: usize,
    ) {
        let Some(node) = pub_state.get_node(node_index) else {
            return;
        };

        // Skip root in display (start from children)
        if depth == 0 {
            if let Some(children) = pub_state.children(node_index) {
                for child in children {
                    if let Some(child_idx) = pub_state.tree.get_index(&child.address) {
                        self.render_toc_tree(ui, pub_state, state, child_idx, 1);
                    }
                }
            }
            return;
        }

        let indent = (depth - 1) * 16;
        let is_branch = node.is_branch();
        let is_expanded = state.expanded_branches.contains(&node_index);
        let is_resolved = node.is_resolved();
        let title = node.display_title();

        ui.horizontal(|ui| {
            ui.add_space(indent as f32);

            // Expand/collapse button for branches
            if is_branch {
                let btn_text = if is_expanded { "▼" } else { "▶" };
                if ui.small_button(btn_text).clicked() {
                    if is_expanded {
                        state.expanded_branches.remove(&node_index);
                    } else {
                        state.expanded_branches.insert(node_index);
                    }
                }
            } else {
                ui.add_space(20.0); // Align with branch buttons
            }

            // Status indicator
            let status = if is_resolved { "" } else { " ⏳" };
            let text = format!("{}{}", title, status);

            // Find which leaf index this corresponds to (for navigation)
            let leaf_index = pub_state
                .resolved_sections()
                .enumerate()
                .find(|(_, (idx, _))| *idx == node_index)
                .map(|(i, _)| i);

            let is_current = state.mode == ReaderMode::Paginated
                && leaf_index.map(|i| i == state.current_leaf_index).unwrap_or(false);

            let rich_text = if is_current {
                egui::RichText::new(text)
                    .strong()
                    .color(ui.visuals().selection.stroke.color)
            } else {
                egui::RichText::new(text)
            };

            if ui
                .add(egui::Label::new(rich_text).sense(egui::Sense::click()))
                .clicked()
            {
                if let Some(idx) = leaf_index {
                    state.current_leaf_index = idx;
                    state.mode = ReaderMode::Paginated;
                    state.toc_visible = false;
                }
            }
        });

        ui.add_space(4.0);

        // Render children if expanded
        if is_branch && is_expanded {
            if let Some(children) = pub_state.children(node_index) {
                for child in children {
                    if let Some(child_idx) = pub_state.tree.get_index(&child.address) {
                        self.render_toc_tree(ui, pub_state, state, child_idx, depth + 1);
                    }
                }
            }
        }
    }

    fn get_author_name(&self, txn: &Transaction, note: &nostrdb::Note) -> Option<String> {
        let pubkey = note.pubkey();
        self.ndb
            .get_profile_by_pubkey(txn, pubkey)
            .ok()
            .and_then(|p| {
                let name = notedeck::name::get_display_name(Some(&p));
                let s = name.name();
                if s == "??" {
                    None
                } else {
                    Some(s.to_string())
                }
            })
    }

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
