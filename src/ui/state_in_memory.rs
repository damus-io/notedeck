use std::any::Any;

use super::global_popup::GlobalPopupType;

/// StateInMemory is a helper struct for interacting with egui memory persisted data
#[derive(Clone)]
pub struct StateInMemory<T: 'static + Clone + Send> {
    id: &'static str,
    default_state: T,
}

impl<T: 'static + Any + Clone + Send + Sync> StateInMemory<T> {
    pub fn get_state(&self, ctx: &egui::Context) -> T {
        ctx.data_mut(|d| {
            d.get_temp(egui::Id::new(self.id))
                .unwrap_or(self.default_state.clone())
        })
    }

    pub fn set_state(&self, ctx: &egui::Context, new_val: T) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(self.id), new_val));
    }
}

pub static STATE_ACCOUNT_MANAGEMENT: StateInMemory<bool> = StateInMemory::<bool> {
    id: ACCOUNT_MANAGEMENT_VIEW_STATE_ID,
    default_state: false,
};

pub static STATE_SIDE_PANEL: StateInMemory<Option<GlobalPopupType>> =
    StateInMemory::<Option<GlobalPopupType>> {
        id: SIDE_PANEL_VIEW_STATE_ID,
        default_state: None,
    };

pub static STATE_GLOBAL_POPUP: StateInMemory<bool> = StateInMemory::<bool> {
    id: GLOBAL_POPUP_VIEW_STATE_ID,
    default_state: false,
};

static ACCOUNT_MANAGEMENT_VIEW_STATE_ID: &str = "account management view state";
static SIDE_PANEL_VIEW_STATE_ID: &str = "side panel view state";
static GLOBAL_POPUP_VIEW_STATE_ID: &str = "global popup view state";
