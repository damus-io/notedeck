use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct NotedeckSettings {
    pub storage_settings: StorageSettings,
}


#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[derive(Default)]
pub struct StorageSettings {}
