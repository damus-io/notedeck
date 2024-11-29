use enostr::FullKeypair;
use nostrdb::{Note, NoteBuilder, NoteReply};
use std::collections::HashSet;

pub struct NewPost {
    pub content: String,
    pub account: FullKeypair,
}

fn add_client_tag(builder: NoteBuilder<'_>) -> NoteBuilder<'_> {
    builder
        .start_tag()
        .tag_str("client")
        .tag_str("Damus Notedeck")
}

impl NewPost {
    pub fn new(content: String, account: FullKeypair) -> Self {
        NewPost { content, account }
    }

    pub fn to_note(&self, seckey: &[u8; 32]) -> Note {
        add_client_tag(NoteBuilder::new())
            .kind(1)
            .content(&self.content)
            .sign(seckey)
            .build()
            .expect("note should be ok")
    }

    pub fn to_reply(&self, seckey: &[u8; 32], replying_to: &Note) -> Note {
        let builder = add_client_tag(NoteBuilder::new())
            .kind(1)
            .content(&self.content);

        let nip10 = NoteReply::new(replying_to.tags());

        let mut builder = if let Some(root) = nip10.root() {
            builder
                .start_tag()
                .tag_str("e")
                .tag_str(&hex::encode(root.id))
                .tag_str("")
                .tag_str("root")
                .start_tag()
                .tag_str("e")
                .tag_str(&hex::encode(replying_to.id()))
                .tag_str("")
                .tag_str("reply")
                .sign(seckey)
        } else {
            // we're replying to a post that isn't in a thread,
            // just add a single reply-to-root tag
            builder
                .start_tag()
                .tag_str("e")
                .tag_str(&hex::encode(replying_to.id()))
                .tag_str("")
                .tag_str("root")
                .sign(seckey)
        };

        let mut seen_p: HashSet<&[u8; 32]> = HashSet::new();

        builder = builder
            .start_tag()
            .tag_str("p")
            .tag_str(&hex::encode(replying_to.pubkey()));

        seen_p.insert(replying_to.pubkey());

        for tag in replying_to.tags() {
            if tag.count() < 2 {
                continue;
            }

            if tag.get_unchecked(0).variant().str() != Some("p") {
                continue;
            }

            let id = if let Some(id) = tag.get_unchecked(1).variant().id() {
                id
            } else {
                continue;
            };

            if seen_p.contains(id) {
                continue;
            }

            seen_p.insert(id);

            builder = builder.start_tag().tag_str("p").tag_str(&hex::encode(id));
        }

        builder
            .sign(seckey)
            .build()
            .expect("expected build to work")
    }

    pub fn to_quote(&self, seckey: &[u8; 32], quoting: &Note) -> Note {
        let new_content = format!(
            "{}\nnostr:{}",
            self.content,
            enostr::NoteId::new(*quoting.id()).to_bech().unwrap()
        );

        NoteBuilder::new()
            .kind(1)
            .content(&new_content)
            .start_tag()
            .tag_str("q")
            .tag_str(&hex::encode(quoting.id()))
            .start_tag()
            .tag_str("p")
            .tag_str(&hex::encode(quoting.pubkey()))
            .sign(seckey)
            .build()
            .expect("expected build to work")
    }
}
