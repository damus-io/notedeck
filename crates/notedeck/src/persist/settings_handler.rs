use crate::{
    storage::delete_file, timed_serializer::TimedSerializer, DataPath, DataPathType, Directory,
};
use egui::ThemePreference;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

const THEME_FILE: &str = "theme.txt";
const ZOOM_FACTOR_FILE: &str = "zoom_level.json";
const SETTINGS_FILE: &str = "settings.json";

const DEFAULT_THEME: ThemePreference = ThemePreference::Dark;
const DEFAULT_LOCALE: &str = "en-US";
const DEFAULT_ZOOM_FACTOR: f32 = 1.0;
const DEFAULT_SHOW_SOURCE_CLIENT: &str = "hide";
const DEFAULT_SHOW_REPLIES_NEWEST_FIRST: bool = false;
const DEFAULT_OUTBOX_ENABLED: bool = true;

fn default_outbox_enabled() -> bool {
    DEFAULT_OUTBOX_ENABLED
}
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const DEFAULT_NOTE_BODY_FONT_SIZE: f32 = 13.0;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const DEFAULT_NOTE_BODY_FONT_SIZE: f32 = 16.0;

fn deserialize_theme(serialized_theme: &str) -> Option<ThemePreference> {
    match serialized_theme {
        "dark" => Some(ThemePreference::Dark),
        "light" => Some(ThemePreference::Light),
        "system" => Some(ThemePreference::System),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, PartialEq, Clone)]
pub struct Settings {
    pub theme: ThemePreference,
    pub locale: String,
    pub zoom_factor: f32,
    pub show_source_client: String,
    pub show_replies_newest_first: bool,
    pub note_body_font_size: f32,
    #[serde(default = "default_outbox_enabled")]
    pub outbox_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME,
            locale: DEFAULT_LOCALE.to_string(),
            zoom_factor: DEFAULT_ZOOM_FACTOR,
            show_source_client: DEFAULT_SHOW_SOURCE_CLIENT.to_string(),
            show_replies_newest_first: DEFAULT_SHOW_REPLIES_NEWEST_FIRST,
            note_body_font_size: DEFAULT_NOTE_BODY_FONT_SIZE,
            outbox_enabled: DEFAULT_OUTBOX_ENABLED,
        }
    }
}

pub struct SettingsHandler {
    directory: Directory,
    serializer: TimedSerializer<Settings>,
    current_settings: Option<Settings>,
}

impl SettingsHandler {
    fn read_from_theme_file(&self) -> Option<ThemePreference> {
        match self.directory.get_file(THEME_FILE.to_string()) {
            Ok(contents) => deserialize_theme(contents.trim()),
            Err(_) => None,
        }
    }

    fn read_from_zomfactor_file(&self) -> Option<f32> {
        match self.directory.get_file(ZOOM_FACTOR_FILE.to_string()) {
            Ok(contents) => serde_json::from_str::<f32>(&contents).ok(),
            Err(_) => None,
        }
    }

    fn migrate_to_settings_file(&mut self) -> bool {
        let mut settings = Settings::default();
        let mut migrated = false;
        // if theme.txt exists migrate
        if let Some(theme_from_file) = self.read_from_theme_file() {
            info!("migrating theme preference from theme.txt file");
            _ = delete_file(&self.directory.file_path, THEME_FILE.to_string());

            settings.theme = theme_from_file;
            migrated = true;
        } else {
            info!("theme.txt file not found, using default theme");
        };

        // if zoom_factor.txt exists migrate
        if let Some(zom_factor) = self.read_from_zomfactor_file() {
            info!("migrating theme preference from zom_factor file");
            _ = delete_file(&self.directory.file_path, ZOOM_FACTOR_FILE.to_string());

            settings.zoom_factor = zom_factor;
            migrated = true;
        } else {
            info!("zoom_factor.txt exists migrate file not found, using default zoom factor");
        };

        if migrated {
            self.current_settings = Some(settings);
            self.try_save_settings();
        }
        migrated
    }

    pub fn new(path: &DataPath) -> Self {
        let directory = Directory::new(path.path(DataPathType::Setting));
        let serializer =
            TimedSerializer::new(path, DataPathType::Setting, "settings.json".to_owned());

        Self {
            directory,
            serializer,
            current_settings: None,
        }
    }

    pub fn load(mut self) -> Self {
        if self.migrate_to_settings_file() {
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

    pub(crate) fn try_save_settings(&mut self) {
        let settings = self.get_settings_mut().clone();
        self.serializer.try_save(settings);
    }

    pub fn get_settings_mut(&mut self) -> &mut Settings {
        if self.current_settings.is_none() {
            self.current_settings = Some(Settings::default());
        }
        self.current_settings.as_mut().unwrap()
    }

    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.get_settings_mut().theme = theme;
        self.try_save_settings();
    }

    pub fn set_locale<S>(&mut self, locale: S)
    where
        S: Into<String>,
    {
        self.get_settings_mut().locale = locale.into();
        self.try_save_settings();
    }

    pub fn set_zoom_factor(&mut self, zoom_factor: f32) {
        self.get_settings_mut().zoom_factor = zoom_factor;
        self.try_save_settings();
    }

    pub fn set_show_source_client<S>(&mut self, option: S)
    where
        S: Into<String>,
    {
        self.get_settings_mut().show_source_client = option.into();
        self.try_save_settings();
    }

    pub fn set_show_replies_newest_first(&mut self, value: bool) {
        self.get_settings_mut().show_replies_newest_first = value;
        self.try_save_settings();
    }

    pub fn set_note_body_font_size(&mut self, value: f32) {
        self.get_settings_mut().note_body_font_size = value;
        self.try_save_settings();
    }

    pub fn set_outbox_enabled(&mut self, value: bool) {
        self.get_settings_mut().outbox_enabled = value;
        self.try_save_settings();
    }

    pub fn update_batch<F>(&mut self, update_fn: F)
    where
        F: FnOnce(&mut Settings),
    {
        let settings = self.get_settings_mut();
        update_fn(settings);
        self.try_save_settings();
    }

    pub fn update_settings(&mut self, new_settings: Settings) {
        self.current_settings = Some(new_settings);
        self.try_save_settings();
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
            .unwrap_or(DEFAULT_SHOW_REPLIES_NEWEST_FIRST)
    }

    pub fn is_loaded(&self) -> bool {
        self.current_settings.is_some()
    }

    pub fn note_body_font_size(&self) -> f32 {
        self.current_settings
            .as_ref()
            .map(|s| s.note_body_font_size)
            .unwrap_or(DEFAULT_NOTE_BODY_FONT_SIZE)
    }

    pub fn outbox_enabled(&self) -> bool {
        self.current_settings
            .as_ref()
            .map(|s| s.outbox_enabled)
            .unwrap_or(DEFAULT_OUTBOX_ENABLED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_enable_outbox() {
        let settings = Settings::default();
        assert!(settings.outbox_enabled);
    }
}
