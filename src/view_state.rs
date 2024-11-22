use std::collections::HashMap;

use crate::login_manager::AcquireKeyState;

/// Various state for views
#[derive(Default)]
pub struct ViewState {
    pub login: AcquireKeyState,
    pub id_state_map: HashMap<egui::Id, AcquireKeyState>,
    pub id_string_map: HashMap<egui::Id, String>,
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut AcquireKeyState {
        &mut self.login
    }
}
