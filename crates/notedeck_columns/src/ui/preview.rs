use notedeck::AppAction;

pub struct PreviewConfig {
    pub is_mobile: bool,
}

pub trait Preview {
    type Prev: notedeck::App;

    fn preview(cfg: PreviewConfig) -> Self::Prev;
}

pub struct PreviewApp {
    view: Box<dyn notedeck::App>,
}

impl PreviewApp {
    pub fn new(view: impl notedeck::App + 'static) -> PreviewApp {
        let view = Box::new(view);
        Self { view }
    }
}

impl notedeck::App for PreviewApp {
    fn update(
        &mut self,
        app_ctx: &mut notedeck::AppContext<'_>,
        ui: &mut egui::Ui,
    ) -> Option<AppAction> {
        self.view.update(app_ctx, ui);
        None
    }
}
