use notedeck::app_creation::setup_cc;

pub struct EguiTestSetup {}

pub trait EguiTestCase: eframe::App {
    fn new(supr: EguiTestSetup) -> Self;
}

impl EguiTestSetup {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cc(cc);

        EguiTestSetup {}
    }
}
