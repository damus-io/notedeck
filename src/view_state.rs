use std::collections::HashMap;

use crate::deck_state::DeckState;
use crate::login_manager::AcquireKeyState;

/// Various state for views
#[derive(Default)]
pub struct ViewState {
    pub login: AcquireKeyState,
    pub id_to_deck_state: HashMap<egui::Id, DeckState>,
    pub id_state_map: HashMap<egui::Id, AcquireKeyState>,
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut AcquireKeyState {
        &mut self.login
    }
}
