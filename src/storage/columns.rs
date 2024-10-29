use tracing::{error, info};

use crate::column::SerializableColumns;

use super::{write_file, DataPaths, Directory};

static COLUMNS_FILE: &str = "columns.json";

pub fn save_columns(columns: SerializableColumns) {
    let serialized_columns = match serde_json::to_string(&columns) {
        Ok(s) => s,
        Err(e) => {
            error!("Could not serialize columns: {}", e);
            return;
        }
    };

    let data_path = match DataPaths::Setting.get_path() {
        Ok(s) => s,
        Err(e) => {
            error!("Could not get data path: {}", e);
            return;
        }
    };

    if let Err(e) = write_file(&data_path, COLUMNS_FILE.to_string(), &serialized_columns) {
        error!("Could not write columns to file {}: {}", COLUMNS_FILE, e);
    } else {
        info!("Successfully wrote columns to {}", COLUMNS_FILE);
    }
}

pub fn load_columns() -> Option<SerializableColumns> {
    let data_path = match DataPaths::Setting.get_path() {
        Ok(s) => s,
        Err(e) => {
            error!("Could not get data path: {}", e);
            return None;
        }
    };

    let columns_string = match Directory::new(data_path).get_file(COLUMNS_FILE.to_owned()) {
        Ok(s) => s,
        Err(e) => {
            error!("Could not read columns from file {}:  {}", COLUMNS_FILE, e);
            return None;
        }
    };

    match serde_json::from_str::<SerializableColumns>(&columns_string) {
        Ok(s) => {
            info!("Successfully loaded columns from {}", COLUMNS_FILE);
            Some(s)
        }
        Err(e) => {
            error!("Could not deserialize columns: {}", e);
            None
        }
    }
}
