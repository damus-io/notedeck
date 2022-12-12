use enostr::{EventId, Profile, Pubkey};
use std::collections::HashMap;

pub struct Contacts {
    pub events: HashMap<Pubkey, EventId>,
    pub profiles: HashMap<Pubkey, Profile>,
}

impl Contacts {
    pub fn new() -> Contacts {
        Contacts {
            events: HashMap::new(),
            profiles: HashMap::new(),
        }
    }
}
