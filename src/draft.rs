use std::collections::HashMap;

#[derive(Default)]
pub struct Draft {
    pub buffer: String,
}

#[derive(Default)]
pub struct Drafts {
    pub replies: HashMap<[u8; 32], Draft>,
    pub compose: Draft,
}

pub enum DraftSource<'a> {
    Compose,
    Reply(&'a [u8; 32]), // note id
}

impl<'a> DraftSource<'a> {
    pub fn draft(&self, drafts: &'a mut Drafts) -> &'a mut Draft {
        match self {
            DraftSource::Compose => &mut drafts.compose,
            DraftSource::Reply(id) => drafts.replies.entry(**id).or_default(),
        }
    }
}

impl Draft {
    pub fn new() -> Self {
        Draft::default()
    }
}
