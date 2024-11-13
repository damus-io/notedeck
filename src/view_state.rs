use std::collections::HashMap;

use crate::{deck_state::DeckState, login_manager::LoginState};

/// Various state for views
#[derive(Default)]
pub struct ViewState {
    pub login: LoginState,
    pub id_to_deck_state: HashMap<egui::Id, DeckState>,
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut LoginState {
        &mut self.login
    }
}
