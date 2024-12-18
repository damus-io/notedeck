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
        let mut builder = add_client_tag(NoteBuilder::new())
            .kind(1)
            .content(&self.content);

        for hashtag in Self::extract_hashtags(&self.content) {
            builder = builder
                .start_tag()
                .tag_str("t")
                .tag_str(&hashtag);
        }

        builder
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

        let mut builder = NoteBuilder::new()
            .kind(1)
            .content(&new_content);

        for hashtag in Self::extract_hashtags(&self.content) {
            builder = builder
                .start_tag()
                .tag_str("t")
                .tag_str(&hashtag);
        }

        builder
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

    fn extract_hashtags(content: &str) -> Vec<String> {
        let mut hashtags = Vec::new();
        for word in content.split_whitespace() {
            if word.starts_with('#') && word.len() > 1 {
                let tag = word[1..].trim_end_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                if !tag.is_empty() {
                    hashtags.push(tag);
                }
            }
        }
        hashtags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hashtags() {
        let test_cases = vec![
            ("Hello #world", vec!["world"]),
            ("Multiple #tags #in #one post", vec!["tags", "in", "one"]),
            ("No hashtags here", vec![]),
            ("#tag1 with #tag2!", vec!["tag1", "tag2"]),
            ("Ignore # empty", vec![]),
            ("Keep #alphanumeric123", vec!["alphanumeric123"]),
        ];

        for (input, expected) in test_cases {
            let result = NewPost::extract_hashtags(input);
            assert_eq!(
                result,
                expected.into_iter().map(String::from).collect::<Vec<_>>(),
                "Failed for input: {}",
                input
            );
        }
    }
}
