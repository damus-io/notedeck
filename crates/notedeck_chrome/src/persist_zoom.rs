use egui::Context;
use notedeck::{DataPath, DataPathType};

use crate::timed_serializer::TimedSerializer;

pub struct ZoomHandler {
    serializer: TimedSerializer<f32>,
}

impl ZoomHandler {
    pub fn new(path: &DataPath) -> Self {
        let serializer =
            TimedSerializer::new(path, DataPathType::Setting, "zoom_level.json".to_owned());

        Self { serializer }
    }

    pub fn try_save_zoom_factor(&mut self, ctx: &Context) {
        let cur_zoom_level = ctx.zoom_factor();
        self.serializer.try_save(cur_zoom_level);
    }

    pub fn get_zoom_factor(&self) -> Option<f32> {
        self.serializer.get_item()
    }
}
