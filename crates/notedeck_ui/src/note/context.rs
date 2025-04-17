use egui::{Rect, Vec2};
use nostrdb::NoteKey;
use notedeck::{BroadcastContext, NoteContextSelection};

pub struct NoteContextButton {
    put_at: Option<Rect>,
    note_key: NoteKey,
}

impl egui::Widget for NoteContextButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let r = if let Some(r) = self.put_at {
            r
        } else {
            let mut place = ui.available_rect_before_wrap();
            let size = Self::max_width();
            place.set_width(size);
            place.set_height(size);
            place
        };

        Self::show(ui, self.note_key, r)
    }
}

impl NoteContextButton {
    pub fn new(note_key: NoteKey) -> Self {
        let put_at: Option<Rect> = None;
        NoteContextButton { note_key, put_at }
    }

    pub fn place_at(mut self, rect: Rect) -> Self {
        self.put_at = Some(rect);
        self
    }

    pub fn max_width() -> f32 {
        Self::max_radius() * 3.0 + Self::max_distance_between_circles() * 2.0
    }

    pub fn size() -> Vec2 {
        let width = Self::max_width();
        egui::vec2(width, width)
    }

    fn max_radius() -> f32 {
        4.0
    }

    fn min_radius() -> f32 {
        2.0
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

    #[profiling::function]
    pub fn show(ui: &mut egui::Ui, note_key: NoteKey, put_at: Rect) -> egui::Response {
        let id = ui.id().with(("more_options_anim", note_key));

        let min_radius = Self::min_radius();
        let anim_speed = 0.05;
        let response = ui.interact(put_at, id, egui::Sense::click());

        let hovered = response.hovered();
        let animation_progress = ui.ctx().animate_bool_with_time(id, hovered, anim_speed);

        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let min_distance = Self::min_distance_between_circles();
        let cur_distance = min_distance
            + (Self::max_distance_between_circles() - min_distance) * animation_progress;

        let cur_radius = min_radius + (Self::max_radius() - min_radius) * animation_progress;

        let center = put_at.center();
        let left_circle_center = center - egui::vec2(cur_distance + cur_radius, 0.0);
        let right_circle_center = center + egui::vec2(cur_distance + cur_radius, 0.0);

        let translated_radius = (cur_radius - 1.0) / 2.0;

        // This works in both themes
        let color = ui.style().visuals.noninteractive().fg_stroke.color;

        // Draw circles
        let painter = ui.painter_at(put_at);
        painter.circle_filled(left_circle_center, translated_radius, color);
        painter.circle_filled(center, translated_radius, color);
        painter.circle_filled(right_circle_center, translated_radius, color);

        response
    }

    #[profiling::function]
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
            if ui.button("Copy note json").clicked() {
                context_selection = Some(NoteContextSelection::CopyNoteJSON);
                ui.close_menu();
            }
            if ui.button("Broadcast").clicked() {
                context_selection = Some(NoteContextSelection::Broadcast(
                    BroadcastContext::Everywhere,
                ));
                ui.close_menu();
            }
            if ui.button("Broadcast to local network").clicked() {
                context_selection = Some(NoteContextSelection::Broadcast(
                    BroadcastContext::LocalNetwork,
                ));
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
