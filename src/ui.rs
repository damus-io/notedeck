pub fn padding<R>(
    amount: f32,
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::none()
        .inner_margin(amount)
        .show(ui, add_contents)
}
