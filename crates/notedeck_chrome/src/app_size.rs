use std::time::Duration;

use egui::Context;

use notedeck::{DataPath, DataPathType};

use crate::timed_serializer::TimedSerializer;

pub struct AppSizeHandler {
    serializer: TimedSerializer<egui::Vec2>,
}

impl AppSizeHandler {
    pub fn new(path: &DataPath) -> Self {
        let serializer =
            TimedSerializer::new(path, DataPathType::Setting, "app_size.json".to_owned())
                .with_delay(Duration::from_millis(500));

        Self { serializer }
    }

    pub fn try_save_app_size(&mut self, ctx: &Context) {
        // There doesn't seem to be a way to check if user is resizing window, so if the rect is different than last saved, we'll wait DELAY before saving again to avoid spamming io
        let cur_size = ctx.input(|i| i.screen_rect.size());
        self.serializer.try_save(cur_size);
    }

    pub fn get_app_size(&self) -> Option<egui::Vec2> {
        self.serializer.get_item()
    }
}
