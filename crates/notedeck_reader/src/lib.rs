//! NKBIP-01 Publications Reader App for Notedeck
//!
//! This crate provides a dedicated reader application for viewing NKBIP-01
//! publications (kind 30040 indices with 30041 content sections).
//!
//! The reader supports multiple viewing modes:
//! - **Outline**: Drill-down navigation through the publication hierarchy
//! - **Continuous**: Scrolling through all sections
//! - **Paginated**: One section at a time

mod nav;
mod state;
mod ui;

pub use nav::PublicationSelection;
pub use state::{PublicationConfig, PublicationTreeState, Publications};
pub use ui::{PublicationNavAction, PublicationView, PublicationViewResponse, ReaderMode};

use enostr::NoteId;
use notedeck::{App, AppAction, AppContext, AppResponse};

/// The Publications Reader application
///
/// A standalone app for reading NKBIP-01 publications with hierarchical
/// navigation support.
pub struct ReaderApp {
    /// Currently selected publication (if any)
    selection: Option<PublicationSelection>,

    /// Publication state manager
    publications: Publications,
}

impl Default for ReaderApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ReaderApp {
    /// Create a new reader app instance
    pub fn new() -> Self {
        Self {
            selection: None,
            publications: Publications::default(),
        }
    }

    /// Open a publication for viewing
    pub fn open(&mut self, index_id: NoteId) {
        self.selection = Some(PublicationSelection::new(index_id));
    }

    /// Close the current publication
    pub fn close(&mut self) {
        self.selection = None;
    }

    /// Check if a publication is currently open
    pub fn is_open(&self) -> bool {
        self.selection.is_some()
    }

    /// Get the current selection (if any)
    pub fn selection(&self) -> Option<&PublicationSelection> {
        self.selection.as_ref()
    }
}

impl App for ReaderApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut app_action: Option<AppAction> = None;
        let mut should_close = false;

        if let Some(selection) = &mut self.selection {
            let response = ui::render_publication(
                selection,
                ctx.ndb,
                ctx.pool,
                &mut self.publications,
                ctx.i18n,
                ctx.relay_info_cache,
                ui,
            );

            // Handle note actions
            if let Some(note_action) = response.action {
                app_action = Some(AppAction::Note(note_action));
            }

            // Handle navigation actions
            if let Some(nav_action) = response.nav_action {
                match nav_action {
                    PublicationNavAction::Back => {
                        selection.navigate_back();
                    }
                    PublicationNavAction::NavigateInto(new_id) => {
                        selection.navigate_into(new_id);
                    }
                    PublicationNavAction::Close => {
                        // Mark for close after exiting the borrow
                        should_close = true;
                        app_action = Some(AppAction::SwitchToColumns);
                    }
                    PublicationNavAction::DrillDown(_)
                    | PublicationNavAction::DrillUp
                    | PublicationNavAction::PrevSibling
                    | PublicationNavAction::NextSibling => {
                        // These are handled within the UI render
                    }
                }
            }
        } else {
            // Empty state - no publication selected
            self.render_empty_state(ui);
        }

        // Close the publication after the borrow ends
        if should_close {
            self.selection = None;
        }

        AppResponse::action(app_action)
    }
}

impl ReaderApp {
    fn render_empty_state(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.heading("Publications Reader");
            ui.add_space(16.0);
            ui.label("Select a publication from the Publications timeline to start reading.");
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("No publication currently open")
                    .color(ui.visuals().weak_text_color()),
            );
        });
    }
}
