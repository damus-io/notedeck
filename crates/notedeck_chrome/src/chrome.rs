
// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;

pub struct Chrome {
    active: i32,
    apps: Vec<Box<dyn notedeck::App>>,
}

impl Chrome {
    pub fn new() -> Self {
        Chrome {
            active: 0,
            apps: vec![],
        }
    }

    pub fn add_app(&mut self, app: impl notedeck::App + 'static) {
        self.apps.push(Box::new(app));
    }

    pub fn set_active(&mut self, app: i32) {
        self.active = app;
    }
}

impl notedeck::App for Chrome {
    fn update(&mut self, ctx: &mut notedeck::AppContext, ui: &mut egui::Ui) {
        let active = self.active;
        self.apps[active as usize].update(ctx, ui);
        //for i in 0..self.apps.len() {
        //    self.apps[i].update(ctx, ui);
        //}
    }
}

