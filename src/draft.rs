use crate::ui::note::PostType;
use std::collections::HashMap;

#[derive(Default)]
pub struct Draft {
    pub buffer: String,
}

#[derive(Default)]
pub struct Drafts {
    replies: HashMap<[u8; 32], Draft>,
    quotes: HashMap<[u8; 32], Draft>,
    compose: Draft,
}

impl Drafts {
    pub fn compose_mut(&mut self) -> &mut Draft {
        &mut self.compose
    }

    pub fn get_from_post_type(&mut self, post_type: &PostType) -> &mut Draft {
        match post_type {
            PostType::New => self.compose_mut(),
            PostType::Quote(note_id) => self.quote_mut(note_id.bytes()),
            PostType::Reply(note_id) => self.reply_mut(note_id.bytes()),
        }
    }

    pub fn reply_mut(&mut self, id: &[u8; 32]) -> &mut Draft {
        self.replies.entry(*id).or_default()
    }

    pub fn quote_mut(&mut self, id: &[u8; 32]) -> &mut Draft {
        self.quotes.entry(*id).or_default()
    }
}

impl Draft {
    pub fn new() -> Self {
        Draft::default()
    }

    pub fn clear(&mut self) {
        self.buffer = "".to_string();
    }
}
