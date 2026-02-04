use std::collections::HashMap;

use crate::cache::ConversationId;
use egui_virtual_list::VirtualList;
use notedeck::NoteRef;

/// Search state for the create conversation UI
#[derive(Default)]
pub struct CreateConvoState {
    pub query: String,
}

#[derive(Default)]
pub struct ConversationStates {
    pub cache: HashMap<ConversationId, ConversationState>,
    pub convos_list: VirtualList,
    pub create_convo: CreateConvoState,
}

impl ConversationStates {
    pub fn new() -> Self {
        let mut convos_list = VirtualList::new();
        convos_list.hide_on_resize(None);
        Self {
            cache: Default::default(),
            convos_list,
            create_convo: Default::default(),
        }
    }
    pub fn get_or_insert(&mut self, id: ConversationId) -> &mut ConversationState {
        self.cache.entry(id).or_default()
    }
}

#[derive(Default)]
pub struct ConversationState {
    pub list: VirtualList,
    pub last_read: Option<NoteRef>,
    pub composer: String,
}
