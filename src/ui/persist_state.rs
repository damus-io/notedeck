use egui::util::id_type_map::SerializableAny;

use super::global_popup::GlobalPopupType;

/// PersistState is a helper struct for interacting with egui memory persisted data
#[derive(Clone)]
pub struct PersistState<T: SerializableAny> {
    id: &'static str,
    default_state: T,
}

impl<T: SerializableAny> PersistState<T> {
    pub fn get_state(&self, ctx: &egui::Context) -> T {
        ctx.data_mut(|d| {
            d.get_persisted(egui::Id::new(self.id))
                .unwrap_or(self.default_state.clone())
        })
    }

    pub fn set_state(&self, ctx: &egui::Context, new_val: T) {
        ctx.data_mut(|d| d.insert_persisted(egui::Id::new(self.id), new_val));
    }
}

pub static PERSISTED_ACCOUNT_MANAGEMENT: PersistState<bool> = PersistState::<bool> {
    id: ACCOUNT_MANAGEMENT_VIEW_STATE_ID,
    default_state: false,
};

pub static PERSISTED_SIDE_PANEL: PersistState<Option<GlobalPopupType>> =
    PersistState::<Option<GlobalPopupType>> {
        id: SIDE_PANEL_VIEW_STATE_ID,
        default_state: None,
    };

pub static PERSISTED_GLOBAL_POPUP: PersistState<bool> = PersistState::<bool> {
    id: GLOBAL_POPUP_VIEW_STATE_ID,
    default_state: false,
};

static ACCOUNT_MANAGEMENT_VIEW_STATE_ID: &str = "account management view state";
static SIDE_PANEL_VIEW_STATE_ID: &str = "side panel view state";
static GLOBAL_POPUP_VIEW_STATE_ID: &str = "global popup view state";
