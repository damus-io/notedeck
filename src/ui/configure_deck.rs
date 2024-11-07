use egui::{vec2, Button, Color32, Label, RichText, Ui, Widget};

use crate::{
    app_style::{deck_icon_font_sized, get_font_size, NotedeckTextStyle, DECK_ICON_SIZE},
    deck_state::DeckState,
    fonts::NamedFontFamily,
};

use super::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    padding,
};

pub struct ConfigureDeckView<'a> {
    state: &'a mut DeckState,
    create_button_text: String,
}

pub struct ConfigureDeckResponse {
    pub icon: char,
    pub name: String,
}

static CREATE_TEXT: &str = "Create Deck";

impl<'a> ConfigureDeckView<'a> {
    pub fn new(state: &'a mut DeckState) -> Self {
        Self {
            state,
            create_button_text: CREATE_TEXT.to_owned(),
        }
    }

    pub fn with_create_text(mut self, text: &str) -> Self {
        self.create_button_text = text.to_owned();
        self
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<ConfigureDeckResponse> {
        let title_font = egui::FontId::new(
            get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4),
            egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
        );
        padding(16.0, ui, |ui| {
            ui.add(Label::new(
                RichText::new("Deck name").font(title_font.clone()),
            ));
            ui.add_space(8.0);
            ui.text_edit_singleline(&mut self.state.deck_name);
            ui.add_space(8.0);
            ui.label("We recommend short names");

            ui.add_space(32.0);
            ui.add(Label::new(RichText::new("Icon").font(title_font)));

            if self.state.selected_glyph.is_none() {
                self.state.selected_glyph = Some('A'); // TODO: get user's selection instead of hard coding
            }

            ui.add(deck_icon(
                ui.id().with("config-deck"),
                self.state.selected_glyph,
                64.0,
                false,
            ));

            if ui
                .add(create_deck_button(&self.create_button_text))
                .clicked()
            {
                if let Some(glyph) = self.state.selected_glyph {
                    Some(ConfigureDeckResponse {
                        icon: glyph,
                        name: self.state.deck_name.clone(),
                    })
                } else {
                    // TODO: error message saying to select glyph
                    None
                }
            } else {
                None
            }
        })
        .inner
    }
}

fn create_deck_button(text: &str) -> impl Widget {
    Button::new(text)
}

pub fn deck_icon(
    id: egui::Id,
    glyph: Option<char>,
    full_size: f32,
    highlight: bool,
) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = full_size * ICON_EXPANSION_MULTIPLE;

        let helper = AnimationHelper::new(ui, id, vec2(max_size, max_size));
        let painter = ui.painter_at(helper.get_animation_rect());
        let bg_center = helper.get_animation_rect().center();

        let (stroke, fill_color) = if highlight {
            (
                ui.visuals().selection.stroke,
                ui.visuals().widgets.inactive.bg_fill,
            )
        } else {
            (
                ui.visuals().widgets.inactive.bg_stroke,
                ui.visuals().widgets.inactive.bg_fill,
            )
        };

        let radius = helper.scale_1d_pos((full_size / 2.0) - stroke.width);
        painter.circle(bg_center, radius, fill_color, stroke);

        if let Some(glyph) = glyph {
            let font = deck_icon_font_sized(helper.scale_1d_pos(DECK_ICON_SIZE));
            let glyph_galley = painter.layout_no_wrap(glyph.to_string(), font, Color32::WHITE);

            let top_left = {
                let mut glyph_rect = glyph_galley.rect;
                glyph_rect.set_center(bg_center);
                glyph_rect.left_top()
            };

            painter.galley(top_left, glyph_galley, Color32::WHITE);
        }

        helper.take_animation_response()
    }
}

mod preview {
    use crate::{
        deck_state::DeckState,
        ui::{Preview, PreviewConfig, View},
    };

    use super::ConfigureDeckView;

    pub struct ConfigureDeckPreview {
        state: DeckState,
    }

    impl ConfigureDeckPreview {
        fn new() -> Self {
            let state = DeckState::default();

            ConfigureDeckPreview { state }
        }
    }

    impl View for ConfigureDeckPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ConfigureDeckView::new(&mut self.state).ui(ui);
        }
    }

    impl<'a> Preview for ConfigureDeckView<'a> {
        type Prev = ConfigureDeckPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            ConfigureDeckPreview::new()
        }
    }
}
