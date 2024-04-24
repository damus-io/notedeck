pub fn hover_expand(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: f32,
    expand_size: f32,
    anim_speed: f32,
) -> (egui::Rect, f32) {
    // Allocate space for the profile picture with a fixed size
    let default_size = size + expand_size;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(default_size, default_size), egui::Sense::hover());

    let val = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered(), anim_speed);

    let size = size + val * expand_size;
    (rect, size)
}
