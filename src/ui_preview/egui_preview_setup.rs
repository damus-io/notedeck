use notedeck::app_creation::setup_cc;

pub struct EguiPreviewSetup {}

pub trait EguiPreviewCase: eframe::App {
    fn new(supr: EguiPreviewSetup) -> Self;
}

impl EguiPreviewSetup {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cc(cc);

        EguiPreviewSetup {}
    }
}
