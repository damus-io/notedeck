use egui::Widget;

use crate::deck_state::DeckState;

use super::configure_deck::{ConfigureDeckResponse, ConfigureDeckView};
use notedeck::{tr, Localization};
use notedeck_ui::padding;

pub struct EditDeckView<'a> {
    config_view: ConfigureDeckView<'a>,
}

pub enum EditDeckResponse {
    Edit(ConfigureDeckResponse),
    Delete,
}

impl<'a> EditDeckView<'a> {
    pub fn new(state: &'a mut DeckState, i18n: &'a mut Localization) -> Self {
        let txt = tr!(i18n, "Edit Deck", "Button label to edit a deck");
        let config_view = ConfigureDeckView::new(state, i18n).with_create_text(txt);
        Self { config_view }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<EditDeckResponse> {
        let mut edit_deck_resp = None;

        padding(egui::Margin::symmetric(16, 4), ui, |ui| {
            if ui.add(delete_button(self.config_view.i18n)).clicked() {
                edit_deck_resp = Some(EditDeckResponse::Delete);
            }
        });

        if let Some(config_resp) = self.config_view.ui(ui) {
            edit_deck_resp = Some(EditDeckResponse::Edit(config_resp))
        }

        edit_deck_resp
    }
}

fn delete_button<'a>(i18n: &'a mut Localization) -> impl Widget + 'a {
    |ui: &mut egui::Ui| {
        let size = egui::vec2(108.0, 40.0);
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.add(
                egui::Button::new(tr!(i18n, "Delete Deck", "Button label to delete a deck"))
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
        ui::{Preview, PreviewConfig},
    };

    use super::EditDeckView;
    use notedeck::{App, AppContext, AppResponse};

    pub struct EditDeckPreview {
        state: DeckState,
    }

    impl EditDeckPreview {
        fn new() -> Self {
            let state = DeckState::default();

            EditDeckPreview { state }
        }
    }

    impl App for EditDeckPreview {
        fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
            EditDeckView::new(&mut self.state, ctx.i18n).ui(ui);
            AppResponse::none()
        }
    }

    impl Preview for EditDeckView<'_> {
        type Prev = EditDeckPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            EditDeckPreview::new()
        }
    }
}
