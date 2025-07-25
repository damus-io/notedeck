use crate::{
    storage::{self, delete_file},
    DataPath, DataPathType, Directory,
};
use egui::ThemePreference;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

const THEME_FILE: &str = "theme.txt";
const SETTINGS_FILE: &str = "settings.json";

const DEFAULT_THEME: ThemePreference = ThemePreference::Dark;
const DEFAULT_LOCALE: &str = "es-US";
const DEFAULT_ZOOM_FACTOR: f32 = 1.0;
const DEFAULT_SHOW_SOURCE_CLIENT: &str = "hide";

fn deserialize_theme(serialized_theme: &str) -> Option<ThemePreference> {
    match serialized_theme {
        "dark" => Some(ThemePreference::Dark),
        "light" => Some(ThemePreference::Light),
        "system" => Some(ThemePreference::System),
        _ => None,
    }
}

#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub theme: ThemePreference,
    pub locale: String,
    pub zoom_factor: f32,
    pub show_source_client: String,
    pub show_replies_newest_first: bool,
}

impl Default for Settings {
    fn default() -> Self {
        // Use the same fallback theme as before
        Self {
            theme: DEFAULT_THEME,
            locale: DEFAULT_LOCALE.to_string(),
            zoom_factor: DEFAULT_ZOOM_FACTOR,
            show_source_client: DEFAULT_SHOW_SOURCE_CLIENT.to_string(),
            show_replies_newest_first: false,
        }
    }
}

pub struct SettingsHandler {
    directory: Directory,
    current_settings: Option<Settings>,
}

impl SettingsHandler {
    fn read_legacy_theme(&self) -> Option<ThemePreference> {
        match self.directory.get_file(THEME_FILE.to_string()) {
            Ok(contents) => deserialize_theme(contents.trim()),
            Err(_) => None,
        }
    }

    fn migrate_to_settings_file(&mut self) -> Result<(), ()> {
        // if theme.txt exists migrate
        if let Some(theme_from_file) = self.read_legacy_theme() {
            info!("migrating theme preference from theme.txt file");
            _ = delete_file(&self.directory.file_path, THEME_FILE.to_string());

            self.current_settings = Some(Settings {
                theme: theme_from_file,
                ..Settings::default()
            });

            self.save();

            Ok(())
        } else {
            Err(())
        }
    }

    pub fn new(path: &DataPath) -> Self {
        let directory = Directory::new(path.path(DataPathType::Setting));
        let current_settings: Option<Settings> = None;

        Self {
            directory,
            current_settings,
        }
    }

    pub fn load(mut self) -> Self {
        if self.migrate_to_settings_file().is_ok() {
            return self;
        }

        match self.directory.get_file(SETTINGS_FILE.to_string()) {
            Ok(contents_str) => {
                // Parse JSON content
                match serde_json::from_str::<Settings>(&contents_str) {
                    Ok(settings) => {
                        self.current_settings = Some(settings);
                    }
                    Err(_) => {
                        error!("Invalid settings format. Using defaults");
                        self.current_settings = Some(Settings::default());
                    }
                }
            }
            Err(_) => {
                error!("Could not read settings. Using defaults");
                self.current_settings = Some(Settings::default());
            }
        }

        self
    }

    pub fn save(&self) {
        let settings = self.current_settings.as_ref().unwrap();
        match serde_json::to_string(settings) {
            Ok(serialized) => {
                if let Err(e) = storage::write_file(
                    &self.directory.file_path,
                    SETTINGS_FILE.to_string(),
                    &serialized,
                ) {
                    error!("Could not save settings: {}", e);
                } else {
                    info!("Settings saved successfully");
                }
            }
            Err(e) => error!("Failed to serialize settings: {}", e),
        };
    }

    pub fn get_settings_mut(&mut self) -> &mut Settings {
        if self.current_settings.is_none() {
            self.current_settings = Some(Settings::default());
        }
        self.current_settings.as_mut().unwrap()
    }

    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.get_settings_mut().theme = theme;
        self.save();
    }

    pub fn set_locale<S>(&mut self, locale: S)
    where
        S: Into<String>,
    {
        self.get_settings_mut().locale = locale.into();
        self.save();
    }

    pub fn set_zoom_factor(&mut self, zoom_factor: f32) {
        self.get_settings_mut().zoom_factor = zoom_factor;
        self.save();
    }

    pub fn set_show_source_client<S>(&mut self, option: S)
    where
        S: Into<String>,
    {
        self.get_settings_mut().show_source_client = option.into();
        self.save();
    }

    pub fn set_show_replies_newest_first(&mut self, value: bool) {
        self.get_settings_mut().show_replies_newest_first = value;
        self.save();
    }

    pub fn update_batch<F>(&mut self, update_fn: F)
    where
        F: FnOnce(&mut Settings),
    {
        let settings = self.get_settings_mut();
        update_fn(settings);
        self.save();
    }

    pub fn update_settings(&mut self, new_settings: Settings) {
        self.current_settings = Some(new_settings);
        self.save();
    }

    pub fn theme(&self) -> ThemePreference {
        self.current_settings
            .as_ref()
            .map(|s| s.theme)
            .unwrap_or(DEFAULT_THEME)
    }

    pub fn locale(&self) -> String {
        self.current_settings
            .as_ref()
            .map(|s| s.locale.clone())
            .unwrap_or_else(|| DEFAULT_LOCALE.to_string())
    }

    pub fn zoom_factor(&self) -> f32 {
        self.current_settings
            .as_ref()
            .map(|s| s.zoom_factor)
            .unwrap_or(DEFAULT_ZOOM_FACTOR)
    }

    pub fn show_source_client(&self) -> String {
        self.current_settings
            .as_ref()
            .map(|s| s.show_source_client.to_string())
            .unwrap_or(DEFAULT_SHOW_SOURCE_CLIENT.to_string())
    }

    pub fn show_replies_newest_first(&self) -> bool {
        self.current_settings
            .as_ref()
            .map(|s| s.show_replies_newest_first)
            .unwrap_or(false)
    }

    pub fn is_loaded(&self) -> bool {
        self.current_settings.is_some()
    }
}
