pub fn hover_expand(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: f32,
    expand_size: f32,
    anim_speed: f32,
) -> (egui::Rect, f32, egui::Response) {
    // Allocate space for the profile picture with a fixed size
    let default_size = size + expand_size;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(default_size, default_size), egui::Sense::click());

    let val = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered(), anim_speed);

    let size = size + val * expand_size;
    (rect, size, response)
}

pub fn hover_expand_small(ui: &mut egui::Ui, id: egui::Id) -> (egui::Rect, f32, egui::Response) {
    let size = 10.0;
    let expand_size = 5.0;
    let anim_speed = 0.05;

    hover_expand(ui, id, size, expand_size, anim_speed)
}
