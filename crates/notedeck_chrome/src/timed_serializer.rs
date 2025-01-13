use std::time::{Duration, Instant};

use notedeck::{storage, DataPath, DataPathType, Directory};
use serde::{Deserialize, Serialize};
use tracing::info;

pub struct TimedSerializer<T: PartialEq + Copy + Serialize + for<'de> Deserialize<'de>> {
    directory: Directory,
    file_name: String,
    delay: Duration,
    last_saved: Instant,
    saved_item: Option<T>,
}

impl<T: PartialEq + Copy + Serialize + for<'de> Deserialize<'de>> TimedSerializer<T> {
    pub fn new(path: &DataPath, path_type: DataPathType, file_name: String) -> Self {
        let directory = Directory::new(path.path(path_type));
        let delay = Duration::from_millis(1000);

        Self {
            directory,
            file_name,
            delay,
            last_saved: Instant::now() - delay,
            saved_item: None,
        }
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn should_save(&self) -> bool {
        self.last_saved.elapsed() >= self.delay
    }

    // returns whether successful
    pub fn try_save(&mut self, cur_item: T) -> bool {
        if self.should_save() {
            if let Some(saved_item) = self.saved_item {
                if saved_item != cur_item {
                    return self.save(cur_item);
                }
            } else {
                return self.save(cur_item);
            }
        }
        false
    }

    pub fn get_item(&self) -> Option<T> {
        if self.saved_item.is_some() {
            return self.saved_item;
        }

        if let Ok(file_contents) = self.directory.get_file(self.file_name.clone()) {
            if let Ok(item) = serde_json::from_str::<T>(&file_contents) {
                return Some(item);
            }
        } else {
            info!("Could not find file {}", self.file_name);
        }

        None
    }

    fn save(&mut self, cur_item: T) -> bool {
        if let Ok(serialized_item) = serde_json::to_string(&cur_item) {
            if storage::write_file(
                &self.directory.file_path,
                self.file_name.clone(),
                &serialized_item,
            )
            .is_ok()
            {
                info!("wrote item {}", serialized_item);
                self.last_saved = Instant::now();
                self.saved_item = Some(cur_item);
                return true;
            }
        }

        false
    }
}
