use std::collections::HashMap;

use enostr::Pubkey;

use crate::deck_state::DeckState;
use crate::gif::GifStateMap;
use crate::login_manager::AcquireKeyState;
use crate::profile_state::ProfileState;

/// Various state for views
#[derive(Default)]
pub struct ViewState {
    pub login: AcquireKeyState,
    pub id_to_deck_state: HashMap<egui::Id, DeckState>,
    pub id_state_map: HashMap<egui::Id, AcquireKeyState>,
    pub id_string_map: HashMap<egui::Id, String>,
    pub pubkey_to_profile_state: HashMap<Pubkey, ProfileState>,
    pub gifs: GifStateMap, // gif url to `GifState`
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut AcquireKeyState {
        &mut self.login
    }
}
