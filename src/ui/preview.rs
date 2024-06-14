use crate::ui::View;

pub struct PreviewConfig {
    pub is_mobile: bool,
}

pub trait Preview {
    type Prev: View;

    fn preview(cfg: PreviewConfig) -> Self::Prev;
}

pub struct PreviewApp {
    view: Box<dyn View>,
}

impl<V> From<V> for PreviewApp
where
    V: View + 'static,
{
    fn from(v: V) -> Self {
        PreviewApp::new(v)
    }
}

impl PreviewApp {
    pub fn new(view: impl View + 'static) -> PreviewApp {
        let view = Box::new(view);
        Self { view }
    }
}

impl eframe::App for PreviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| self.view.ui(ui));
    }
}
