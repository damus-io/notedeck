use crate::{app_style::deck_icon_font_sized, deck_state::DeckState};
use egui::{vec2, Button, Color32, Label, RichText, Stroke, Ui, Widget};
use notedeck::{NamedFontFamily, NotedeckTextStyle};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    colors::PINK,
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
            notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4),
            egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
        );
        padding(16.0, ui, |ui| {
            ui.add(Label::new(
                RichText::new("Deck name").font(title_font.clone()),
            ));
            ui.add_space(8.0);
            ui.text_edit_singleline(&mut self.state.deck_name);
            ui.add_space(8.0);
            ui.add(Label::new(
                RichText::new("We recommend short names")
                    .color(ui.visuals().noninteractive().fg_stroke.color)
                    .size(notedeck::fonts::get_font_size(
                        ui.ctx(),
                        &NotedeckTextStyle::Small,
                    )),
            ));

            ui.add_space(32.0);
            ui.add(Label::new(RichText::new("Icon").font(title_font)));

            if ui
                .add(deck_icon(
                    ui.id().with("config-deck"),
                    self.state.selected_glyph,
                    38.0,
                    64.0,
                    false,
                ))
                .clicked()
            {
                self.state.selecting_glyph = !self.state.selecting_glyph;
            }

            if self.state.selecting_glyph {
                let max_height = if ui.available_height() - 100.0 > 0.0 {
                    ui.available_height() - 100.0
                } else {
                    ui.available_height()
                };
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    let glyphs = self.state.get_glyph_options(ui);
                    if let Some(selected_glyph) = glyph_options_ui(ui, 16.0, max_height, glyphs) {
                        self.state.selected_glyph = Some(selected_glyph);
                        self.state.selecting_glyph = false;
                    }
                });
                ui.add_space(16.0);
            }

            if self.state.warn_no_icon && self.state.selected_glyph.is_some() {
                self.state.warn_no_icon = false;
            }
            if self.state.warn_no_title && !self.state.deck_name.is_empty() {
                self.state.warn_no_title = false;
            }

            show_warnings(ui, self.state.warn_no_icon, self.state.warn_no_title);

            let mut resp = None;
            if ui
                .add(create_deck_button(&self.create_button_text))
                .clicked()
            {
                if self.state.deck_name.is_empty() {
                    self.state.warn_no_title = true;
                }
                if self.state.selected_glyph.is_none() {
                    self.state.warn_no_icon = true;
                }
                if !self.state.deck_name.is_empty() {
                    if let Some(glyph) = self.state.selected_glyph {
                        resp = Some(ConfigureDeckResponse {
                            icon: glyph,
                            name: self.state.deck_name.clone(),
                        });
                    }
                }
            }
            resp
        })
        .inner
    }
}

fn show_warnings(ui: &mut Ui, warn_no_icon: bool, warn_no_title: bool) {
    if warn_no_icon || warn_no_title {
        let messages = [
            if warn_no_title {
                "create a name for the deck"
            } else {
                ""
            },
            if warn_no_icon { "select an icon" } else { "" },
        ];
        let message = messages
            .iter()
            .filter(|&&m| !m.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" and ");

        ui.add(
            egui::Label::new(
                RichText::new(format!("Please {message}.")).color(ui.visuals().error_fg_color),
            )
            .wrap(),
        );
    }
}

fn create_deck_button(text: &str) -> impl Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = vec2(108.0, 40.0);
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.add(Button::new(text).fill(PINK).min_size(size))
        })
        .inner
    }
}

pub fn deck_icon(
    id: egui::Id,
    glyph: Option<char>,
    font_size: f32,
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
                ui.visuals().widgets.noninteractive.weak_bg_fill,
            )
        } else {
            (
                Stroke::new(
                    ui.visuals().widgets.inactive.bg_stroke.width,
                    ui.visuals().widgets.inactive.weak_bg_fill,
                ),
                ui.visuals().widgets.noninteractive.weak_bg_fill,
            )
        };

        let radius = helper.scale_1d_pos((full_size / 2.0) - stroke.width);
        painter.circle(bg_center, radius, fill_color, stroke);

        if let Some(glyph) = glyph {
            let font =
                deck_icon_font_sized(helper.scale_1d_pos(font_size / std::f32::consts::SQRT_2));
            let glyph_galley =
                painter.layout_no_wrap(glyph.to_string(), font, ui.visuals().text_color());

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

fn glyph_icon_max_size(ui: &egui::Ui, glyph: &char, font_size: f32) -> egui::Vec2 {
    let painter = ui.painter();
    let font = deck_icon_font_sized(font_size * ICON_EXPANSION_MULTIPLE);
    let glyph_galley = painter.layout_no_wrap(glyph.to_string(), font, Color32::WHITE);
    glyph_galley.rect.size()
}

fn glyph_icon(glyph: char, font_size: f32, max_size: egui::Vec2, color: Color32) -> impl Widget {
    move |ui: &mut egui::Ui| {
        let helper = AnimationHelper::new(ui, ("glyph", glyph), max_size);
        let painter = ui.painter_at(helper.get_animation_rect());

        let font = deck_icon_font_sized(helper.scale_1d_pos(font_size));
        let glyph_galley = painter.layout_no_wrap(glyph.to_string(), font, color);

        let top_left = {
            let mut glyph_rect = glyph_galley.rect;
            glyph_rect.set_center(helper.get_animation_rect().center());
            glyph_rect.left_top()
        };

        painter.galley(top_left, glyph_galley, Color32::WHITE);
        helper.take_animation_response()
    }
}

fn glyph_options_ui(
    ui: &mut egui::Ui,
    font_size: f32,
    max_height: f32,
    glyphs: &[char],
) -> Option<char> {
    let mut selected_glyph = None;
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| {
            let max_width = ui.available_width();
            let mut row_glyphs = Vec::new();
            let mut cur_width = 0.0;
            let spacing = ui.spacing().item_spacing.x;

            for (index, glyph) in glyphs.iter().enumerate() {
                let next_glyph_size = glyph_icon_max_size(ui, glyph, font_size);

                if cur_width + spacing + next_glyph_size.x > max_width {
                    if let Some(selected) = paint_row(ui, &row_glyphs, font_size) {
                        selected_glyph = Some(selected);
                    }
                    row_glyphs.clear();
                    cur_width = 0.0;
                }

                cur_width += spacing;
                cur_width += next_glyph_size.x;
                row_glyphs.push(*glyph);

                if index == glyphs.len() - 1 {
                    if let Some(selected) = paint_row(ui, &row_glyphs, font_size) {
                        selected_glyph = Some(selected);
                    }
                }
            }
        });
    selected_glyph
}

fn paint_row(ui: &mut egui::Ui, row_glyphs: &[char], font_size: f32) -> Option<char> {
    let mut selected_glyph = None;
    ui.horizontal(|ui| {
        for glyph in row_glyphs {
            let glyph_size = glyph_icon_max_size(ui, glyph, font_size);
            if ui
                .add(glyph_icon(
                    *glyph,
                    font_size,
                    glyph_size,
                    ui.visuals().text_color(),
                ))
                .clicked()
            {
                selected_glyph = Some(*glyph);
            }
        }
    });
    selected_glyph
}

mod preview {
    use crate::{
        deck_state::DeckState,
        ui::{Preview, PreviewConfig},
    };

    use super::ConfigureDeckView;
    use notedeck::{App, AppAction, AppContext};

    pub struct ConfigureDeckPreview {
        state: DeckState,
    }

    impl ConfigureDeckPreview {
        fn new() -> Self {
            let state = DeckState::default();

            ConfigureDeckPreview { state }
        }
    }

    impl App for ConfigureDeckPreview {
        fn update(
            &mut self,
            _app_ctx: &mut AppContext<'_>,
            ui: &mut egui::Ui,
        ) -> Option<AppAction> {
            ConfigureDeckView::new(&mut self.state).ui(ui);

            None
        }
    }

    impl Preview for ConfigureDeckView<'_> {
        type Prev = ConfigureDeckPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            ConfigureDeckPreview::new()
        }
    }
}
