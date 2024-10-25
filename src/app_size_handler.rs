use std::time::{Duration, Instant};

use egui::Context;
use tracing::{error, info};

use crate::{
    storage::{write_file, Directory},
    DataPaths,
};

pub struct AppSizeHandler {
    directory: Option<Directory>,
    saved_size: Option<egui::Vec2>,
    last_saved: Instant,
}

static FILE_NAME: &str = "app_size.json";
static DELAY: Duration = Duration::from_millis(500);

impl Default for AppSizeHandler {
    fn default() -> Self {
        let directory = match DataPaths::Setting.get_path() {
            Ok(path) => Some(Directory::new(path)),
            Err(e) => {
                error!("Could not load settings path: {}", e);
                None
            }
        };

        Self {
            directory,
            saved_size: None,
            last_saved: Instant::now() - DELAY,
        }
    }
}

impl AppSizeHandler {
    pub fn try_save_app_size(&mut self, ctx: &Context) {
        if let Some(interactor) = &self.directory {
            // There doesn't seem to be a way to check if user is resizing window, so if the rect is different than last saved, we'll wait DELAY before saving again to avoid spamming io
            if self.last_saved.elapsed() >= DELAY {
                internal_try_save_app_size(interactor, &mut self.saved_size, ctx);
                self.last_saved = Instant::now();
            }
        }
    }

    pub fn get_app_size(&self) -> Option<egui::Vec2> {
        if self.saved_size.is_some() {
            return self.saved_size;
        }

        if let Some(directory) = &self.directory {
            if let Ok(file_contents) = directory.get_file(FILE_NAME.to_owned()) {
                if let Ok(rect) = serde_json::from_str::<egui::Vec2>(&file_contents) {
                    return Some(rect);
                }
            } else {
                info!("Could not find {}", FILE_NAME);
            }
        }

        None
    }
}

fn internal_try_save_app_size(
    interactor: &Directory,
    maybe_saved_size: &mut Option<egui::Vec2>,
    ctx: &Context,
) {
    let cur_size = ctx.input(|i| i.screen_rect.size());
    if let Some(saved_size) = maybe_saved_size {
        if cur_size != *saved_size {
            try_save_size(interactor, cur_size, maybe_saved_size);
        }
    } else {
        try_save_size(interactor, cur_size, maybe_saved_size);
    }
}

fn try_save_size(
    interactor: &Directory,
    cur_size: egui::Vec2,
    maybe_saved_size: &mut Option<egui::Vec2>,
) {
    if let Ok(serialized_rect) = serde_json::to_string(&cur_size) {
        if write_file(
            &interactor.file_path,
            FILE_NAME.to_owned(),
            &serialized_rect,
        )
        .is_ok()
        {
            info!("wrote size {}", cur_size,);
            *maybe_saved_size = Some(cur_size);
        }
    }
}
