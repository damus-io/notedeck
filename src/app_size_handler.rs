use std::time::{Duration, Instant};

use egui::Context;
use tracing::{error, info};

use crate::{storage::FileDirectoryInteractor, FileWriterFactory};

pub struct AppSizeHandler {
    interactor: Option<FileDirectoryInteractor>,
    saved_size: Option<egui::Vec2>,
    last_saved: Instant,
}

static FILE_NAME: &str = "app_size.json";
static DELAY: Duration = Duration::from_millis(500);

impl Default for AppSizeHandler {
    fn default() -> Self {
        let interactor = FileWriterFactory::new(crate::FileWriterType::Setting)
            .build()
            .ok();
        if interactor.is_none() {
            error!("Failed to create Settings FileDirectoryInteractor");
        }
        Self {
            interactor,
            saved_size: None,
            last_saved: Instant::now() - DELAY,
        }
    }
}

impl AppSizeHandler {
    pub fn try_save_app_size(&mut self, ctx: &Context) {
        if let Some(interactor) = &self.interactor {
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

        if let Some(interactor) = &self.interactor {
            if let Ok(file_contents) = interactor.get_file(FILE_NAME.to_owned()) {
                if let Ok(rect) = serde_json::from_str::<egui::Vec2>(&file_contents) {
                    return Some(rect);
                }
            } else {
                info!(
                    "Could not find {} in {:?}",
                    FILE_NAME,
                    interactor.get_directory()
                );
            }
        }

        None
    }
}

fn internal_try_save_app_size(
    interactor: &FileDirectoryInteractor,
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
    interactor: &FileDirectoryInteractor,
    cur_size: egui::Vec2,
    maybe_saved_size: &mut Option<egui::Vec2>,
) {
    if let Ok(serialized_rect) = serde_json::to_string(&cur_size) {
        if interactor
            .write(FILE_NAME.to_owned(), &serialized_rect)
            .is_ok()
        {
            info!("wrote size {}", cur_size,);
            *maybe_saved_size = Some(cur_size);
        }
    }
}
