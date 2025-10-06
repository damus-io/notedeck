/// Context menu helpers (paste, etc)
use egui_winit::clipboard::Clipboard;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PasteBehavior {
    Clear,
    Append,
}

fn handle_paste(clipboard: &mut Clipboard, input: &mut String, paste_behavior: PasteBehavior) {
    if let Some(text) = clipboard.get() {
        // if called with clearing_input_context, then we clear before
        // we paste. Useful for certain fields like passwords, etc
        match paste_behavior {
            PasteBehavior::Clear => input.clear(),
            PasteBehavior::Append => {}
        }
        input.push_str(&text);
    }
}

pub fn input_context(
    ui: &mut egui::Ui,
    response: &egui::Response,
    clipboard: &mut Clipboard,
    input: &mut String,
    paste_behavior: PasteBehavior,
) {
    response.context_menu(|ui| {
        if ui.button("Paste").clicked() {
            handle_paste(clipboard, input, paste_behavior);
            ui.close_menu();
        }

        if ui.button("Copy").clicked() {
            clipboard.set_text(input.to_owned());
            ui.close_menu();
        }

        if ui.button("Cut").clicked() {
            clipboard.set_text(input.to_owned());
            input.clear();
            ui.close_menu();
        }
    });

    if response.middle_clicked() {
        handle_paste(clipboard, input, paste_behavior)
    }

    // for keyboard visibility
    crate::include_input(ui, response)
}

pub fn stationary_arbitrary_menu_button<R>(
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

pub fn context_button(ui: &mut egui::Ui, id: egui::Id, put_at: egui::Rect) -> egui::Response {
    let min_radius = 2.0;
    let anim_speed = 0.05;
    let response = ui.interact(put_at, id, egui::Sense::click());

    let hovered = response.hovered();
    let animation_progress = ui.ctx().animate_bool_with_time(id, hovered, anim_speed);

    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let max_distance = 2.0;
    let expansion_mult = 2.0;
    let min_distance = max_distance / expansion_mult;
    let cur_distance = min_distance + (max_distance - min_distance) * animation_progress;

    let max_radius = 4.0;
    let cur_radius = min_radius + (max_radius - min_radius) * animation_progress;

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
