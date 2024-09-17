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

    pub fn reply_mut(&mut self, id: &[u8; 32]) -> &mut Draft {
        self.replies.entry(*id).or_default()
    }

    pub fn quote_mut(&mut self, id: &[u8; 32]) -> &mut Draft {
        self.quotes.entry(*id).or_default()
    }
}

pub enum DraftSource<'a> {
    Compose,
    Reply(&'a [u8; 32]), // note id
    Quote(&'a [u8; 32]), // note id
}

/*
impl<'a> DraftSource<'a> {
    pub fn draft(&self, drafts: &'a mut Drafts) -> &'a mut Draft {
        match self {
            DraftSource::Compose => drafts.compose_mut(),
            DraftSource::Reply(id) => drafts.reply_mut(id),
        }
    }
}
*/

impl Draft {
    pub fn new() -> Self {
        Draft::default()
    }

    pub fn clear(&mut self) {
        self.buffer = "".to_string();
    }
}
