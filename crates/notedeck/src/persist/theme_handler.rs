use egui::ThemePreference;
use tracing::{error, info};

use crate::{storage, DataPath, DataPathType, Directory};

pub struct ThemeHandler {
    directory: Directory,
    fallback_theme: ThemePreference,
}

const THEME_FILE: &str = "theme.txt";

impl ThemeHandler {
    pub fn new(path: &DataPath) -> Self {
        let directory = Directory::new(path.path(DataPathType::Setting));
        let fallback_theme = ThemePreference::Dark;
        Self {
            directory,
            fallback_theme,
        }
    }

    pub fn load(&self) -> ThemePreference {
        match self.directory.get_file(THEME_FILE.to_owned()) {
            Ok(contents) => match deserialize_theme(contents) {
                Some(theme) => theme,
                None => {
                    error!(
                        "Could not deserialize theme. Using fallback {:?} instead",
                        self.fallback_theme
                    );
                    self.fallback_theme
                }
            },
            Err(e) => {
                error!(
                    "Could not read {} file: {:?}\nUsing fallback {:?} instead",
                    THEME_FILE, e, self.fallback_theme
                );
                self.fallback_theme
            }
        }
    }

    pub fn save(&self, theme: ThemePreference) {
        match storage::write_file(
            &self.directory.file_path,
            THEME_FILE.to_owned(),
            &theme_to_serialized(&theme),
        ) {
            Ok(_) => info!(
                "Successfully saved {:?} theme change to {}",
                theme, THEME_FILE
            ),
            Err(_) => error!("Could not save {:?} theme change to {}", theme, THEME_FILE),
        }
    }
}

fn theme_to_serialized(theme: &ThemePreference) -> String {
    match theme {
        ThemePreference::Dark => "dark",
        ThemePreference::Light => "light",
        ThemePreference::System => "system",
    }
    .to_owned()
}

fn deserialize_theme(serialized_theme: String) -> Option<ThemePreference> {
    match serialized_theme.as_str() {
        "dark" => Some(ThemePreference::Dark),
        "light" => Some(ThemePreference::Light),
        "system" => Some(ThemePreference::System),
        _ => None,
    }
}
