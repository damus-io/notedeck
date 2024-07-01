use nostrdb::NoteBuilder;

pub struct NewPost {
    pub content: String,
    pub account: usize,
}

impl NewPost {
    pub fn to_note(&self, seckey: &[u8; 32]) -> nostrdb::Note {
        NoteBuilder::new()
            .kind(1)
            .content(&self.content)
            .sign(seckey)
            .build()
            .expect("note should be ok")
    }
}
