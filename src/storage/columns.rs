use tracing::{error, info};

use crate::decks::SerializableDecksCache;

use super::{write_file, DataPath, DataPathType, Directory};

static DECKS_CACHE_FILE: &str = "decks_cache.json";

pub fn load_decks_cache(path: &DataPath) -> Option<SerializableDecksCache> {
    let data_path = path.path(DataPathType::Setting);

    let decks_cache_str = match Directory::new(data_path).get_file(DECKS_CACHE_FILE.to_owned()) {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Could not read decks cache from file {}:  {}",
                DECKS_CACHE_FILE, e
            );
            return None;
        }
    };

    match serde_json::from_str::<SerializableDecksCache>(&decks_cache_str) {
        Ok(s) => {
            info!("Successfully loaded decks cache from {}", DECKS_CACHE_FILE);
            Some(s)
        }
        Err(e) => {
            error!("Could not deserialize decks cache: {}", e);
            None
        }
    }
}

pub fn save_decks_cache(path: &DataPath, decks_cache: &SerializableDecksCache) {
    let serialized_decks_cache = match serde_json::to_string(decks_cache) {
        Ok(s) => s,
        Err(e) => {
            error!("Could not serialize decks cache: {}", e);
            return;
        }
    };

    let data_path = path.path(DataPathType::Setting);

    if let Err(e) = write_file(
        &data_path,
        DECKS_CACHE_FILE.to_string(),
        &serialized_decks_cache,
    ) {
        error!(
            "Could not write decks cache to file {}: {}",
            DECKS_CACHE_FILE, e
        );
    } else {
        info!("Successfully wrote decks cache to {}", DECKS_CACHE_FILE);
    }
}
