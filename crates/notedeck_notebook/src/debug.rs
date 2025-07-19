
/*
fn debug_slider(
    ui: &mut egui::Ui,
    id: egui::Id,
    point: Pos2,
    initial: f32,
    range: std::ops::RangeInclusive<f32>,
) -> f32 {
    let mut val = ui.data_mut(|d| *d.get_temp_mut_or::<f32>(id, initial));
    let nudge = vec2(10.0, 10.0);
    let slider = Rect::from_min_max(point - nudge, point + nudge);
    let label = Rect::from_min_max(point + nudge * 2.0, point - nudge * 2.0);

    let old_val = val;
    ui.put(slider, egui::Slider::new(&mut val, range));
    ui.put(label, egui::Label::new(format!("{val}")));

    if val != old_val {
        ui.data_mut(|d| d.insert_temp(id, val))
    }

    val
}
*/

