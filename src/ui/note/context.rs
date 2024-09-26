use crate::colors;
use egui::Vec2;
use enostr::{NoteId, Pubkey};
use nostrdb::{Note, NoteKey};

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub enum NoteContextSelection {
    CopyText,
    CopyPubkey,
    CopyNoteId,
}

impl NoteContextSelection {
    pub fn process(&self, ui: &mut egui::Ui, note: &Note<'_>) {
        match self {
            NoteContextSelection::CopyText => {
                ui.output_mut(|w| {
                    w.copied_text = note.content().to_string();
                });
            }
            NoteContextSelection::CopyPubkey => {
                ui.output_mut(|w| {
                    if let Some(bech) = Pubkey::new(*note.pubkey()).to_bech() {
                        w.copied_text = bech;
                    }
                });
            }
            NoteContextSelection::CopyNoteId => {
                ui.output_mut(|w| {
                    if let Some(bech) = NoteId::new(*note.id()).to_bech() {
                        w.copied_text = bech;
                    }
                });
            }
        }
    }
}

pub struct NoteContextButton {
    note_key: NoteKey,
}

impl egui::Widget for NoteContextButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        Self::show(ui, self.note_key)
    }
}

impl NoteContextButton {
    pub fn new(note_key: NoteKey) -> Self {
        NoteContextButton { note_key }
    }

    pub fn max_width() -> f32 {
        Self::max_radius() * 3.0 + Self::max_distance_between_circles() * 2.0
    }

    pub fn size() -> Vec2 {
        let width = Self::max_width();
        egui::vec2(width, width)
    }

    fn max_radius() -> f32 {
        8.0
    }

    fn min_radius() -> f32 {
        Self::max_radius() / Self::expansion_multiple()
    }

    fn max_distance_between_circles() -> f32 {
        2.0
    }

    fn expansion_multiple() -> f32 {
        2.0
    }

    fn min_distance_between_circles() -> f32 {
        Self::max_distance_between_circles() / Self::expansion_multiple()
    }

    pub fn show(ui: &mut egui::Ui, note_key: NoteKey) -> egui::Response {
        let id = ui.id().with(("more_options_anim", note_key));

        let min_radius = Self::min_radius();
        let anim_speed = 0.05;
        let size = Self::size();
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

        let animation_progress =
            ui.ctx()
                .animate_bool_with_time(id, response.hovered(), anim_speed);

        let min_distance = Self::min_distance_between_circles();
        let cur_distance = min_distance
            + (Self::max_distance_between_circles() - min_distance) * animation_progress;

        let cur_radius = min_radius + (Self::max_radius() - min_radius) * animation_progress;

        let center = rect.center();
        let left_circle_center = center - egui::vec2(cur_distance + cur_radius, 0.0);
        let right_circle_center = center + egui::vec2(cur_distance + cur_radius, 0.0);

        let translated_radius = (cur_radius - 1.0) / 2.0;

        // This works in both themes
        let color = colors::GRAY_SECONDARY;

        // Draw circles
        let painter = ui.painter_at(rect);
        painter.circle_filled(left_circle_center, translated_radius, color);
        painter.circle_filled(center, translated_radius, color);
        painter.circle_filled(right_circle_center, translated_radius, color);

        response
    }

    pub fn menu(
        ui: &mut egui::Ui,
        button_response: egui::Response,
    ) -> Option<NoteContextSelection> {
        let mut context_selection: Option<NoteContextSelection> = None;

        stationary_arbitrary_menu_button(ui, button_response, |ui| {
            ui.set_max_width(200.0);
            if ui.button("Copy text").clicked() {
                context_selection = Some(NoteContextSelection::CopyText);
                ui.close_menu();
            }
            if ui.button("Copy user public key").clicked() {
                context_selection = Some(NoteContextSelection::CopyPubkey);
                ui.close_menu();
            }
            if ui.button("Copy note id").clicked() {
                context_selection = Some(NoteContextSelection::CopyNoteId);
                ui.close_menu();
            }
        });

        context_selection
    }
}

fn stationary_arbitrary_menu_button<R>(
    ui: &mut egui::Ui,
    button_response: egui::Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let bar_id = ui.id();
    let mut bar_state = egui::menu::BarState::load(ui.ctx(), bar_id);

    let inner = bar_state.bar_menu(&button_response, add_contents);

    bar_state.store(ui.ctx(), bar_id);
    egui::InnerResponse::new(inner.map(|r| r.inner), button_response)
}
