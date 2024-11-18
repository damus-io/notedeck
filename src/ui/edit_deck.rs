use egui::Widget;

use crate::deck_state::DeckState;

use super::{
    configure_deck::{ConfigureDeckResponse, ConfigureDeckView},
    padding,
};

pub struct EditDeckView<'a> {
    config_view: ConfigureDeckView<'a>,
}

static EDIT_TEXT: &str = "Edit Deck";

pub enum EditDeckResponse {
    Edit(ConfigureDeckResponse),
    Delete,
}

impl<'a> EditDeckView<'a> {
    pub fn new(state: &'a mut DeckState) -> Self {
        let config_view = ConfigureDeckView::new(state).with_create_text(EDIT_TEXT);
        Self { config_view }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<EditDeckResponse> {
        let mut edit_deck_resp = None;

        padding(egui::Margin::symmetric(16.0, 4.0), ui, |ui| {
            if ui.add(delete_button()).clicked() {
                edit_deck_resp = Some(EditDeckResponse::Delete);
            }
        });

        if let Some(config_resp) = self.config_view.ui(ui) {
            edit_deck_resp = Some(EditDeckResponse::Edit(config_resp))
        }

        edit_deck_resp
    }
}

fn delete_button() -> impl Widget {
    |ui: &mut egui::Ui| {
        let size = egui::vec2(108.0, 40.0);
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.add(
                egui::Button::new("Delete Deck")
                    .fill(ui.visuals().error_fg_color)
                    .min_size(size),
            )
        })
        .inner
    }
}

mod preview {
    use crate::{
        deck_state::DeckState,
        ui::{Preview, PreviewConfig, View},
    };

    use super::EditDeckView;

    pub struct EditDeckPreview {
        state: DeckState,
    }

    impl EditDeckPreview {
        fn new() -> Self {
            let state = DeckState::default();

            EditDeckPreview { state }
        }
    }

    impl View for EditDeckPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            EditDeckView::new(&mut self.state).ui(ui);
        }
    }

    impl<'a> Preview for EditDeckView<'a> {
        type Prev = EditDeckPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            EditDeckPreview::new()
        }
    }
}
