use enostr::FullKeypair;
use nostrdb::{Note, NoteBuilder, NoteReply};
use std::collections::HashSet;

use crate::media_upload::Nip94Event;

pub struct NewPost {
    pub content: String,
    pub account: FullKeypair,
    pub media: Vec<Nip94Event>,
}

fn add_client_tag(builder: NoteBuilder<'_>) -> NoteBuilder<'_> {
    builder
        .start_tag()
        .tag_str("client")
        .tag_str("Damus Notedeck")
}

impl NewPost {
    pub fn new(content: String, account: FullKeypair, media: Vec<Nip94Event>) -> Self {
        NewPost {
            content,
            account,
            media,
        }
    }

    pub fn to_note(&self, seckey: &[u8; 32]) -> Note {
        let mut content = self.content.clone();
        append_urls(&mut content, &self.media);

        let mut builder = add_client_tag(NoteBuilder::new()).kind(1).content(&content);

        for hashtag in Self::extract_hashtags(&self.content) {
            builder = builder.start_tag().tag_str("t").tag_str(&hashtag);
        }

        if !self.media.is_empty() {
            builder = add_imeta_tags(builder, &self.media);
        }

        builder.sign(seckey).build().expect("note should be ok")
    }

    pub fn to_reply(&self, seckey: &[u8; 32], replying_to: &Note) -> Note {
        let mut content = self.content.clone();
        append_urls(&mut content, &self.media);

        let builder = add_client_tag(NoteBuilder::new()).kind(1).content(&content);

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

        if !self.media.is_empty() {
            builder = add_imeta_tags(builder, &self.media);
        }

        builder
            .sign(seckey)
            .build()
            .expect("expected build to work")
    }

    pub fn to_quote(&self, seckey: &[u8; 32], quoting: &Note) -> Note {
        let mut new_content = format!(
            "{}\nnostr:{}",
            self.content,
            enostr::NoteId::new(*quoting.id()).to_bech().unwrap()
        );

        append_urls(&mut new_content, &self.media);

        let mut builder = NoteBuilder::new().kind(1).content(&new_content);

        for hashtag in Self::extract_hashtags(&self.content) {
            builder = builder.start_tag().tag_str("t").tag_str(&hashtag);
        }

        if !self.media.is_empty() {
            builder = add_imeta_tags(builder, &self.media);
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

    fn extract_hashtags(content: &str) -> HashSet<String> {
        let mut hashtags = HashSet::new();
        for word in
            content.split(|c: char| c.is_whitespace() || (c.is_ascii_punctuation() && c != '#'))
        {
            if word.starts_with('#') && word.len() > 1 {
                let tag = word[1..].to_lowercase();
                if !tag.is_empty() {
                    hashtags.insert(tag);
                }
            }
        }
        hashtags
    }
}

fn append_urls(content: &mut String, media: &Vec<Nip94Event>) {
    for ev in media {
        content.push(' ');
        content.push_str(&ev.url);
    }
}

fn add_imeta_tags<'a>(builder: NoteBuilder<'a>, media: &Vec<Nip94Event>) -> NoteBuilder<'a> {
    let mut builder = builder;
    for item in media {
        builder = builder
            .start_tag()
            .tag_str("imeta")
            .tag_str(&format!("url {}", item.url));

        if let Some(ox) = &item.ox {
            builder = builder.tag_str(&format!("ox {ox}"));
        };
        if let Some(x) = &item.x {
            builder = builder.tag_str(&format!("x {x}"));
        }
        if let Some(media_type) = &item.media_type {
            builder = builder.tag_str(&format!("m {media_type}"));
        }
        if let Some(dims) = &item.dimensions {
            builder = builder.tag_str(&format!("dim {}x{}", dims.0, dims.1));
        }
        if let Some(bh) = &item.blurhash {
            builder = builder.tag_str(&format!("blurhash {bh}"));
        }
        if let Some(thumb) = &item.thumb {
            builder = builder.tag_str(&format!("thumb {thumb}"));
        }
    }
    builder
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
            ("Testing emoji #🍌banana", vec!["🍌banana"]),
            ("Testing emoji #🍌", vec!["🍌"]),
            ("Duplicate #tag #tag #tag", vec!["tag"]),
            ("Mixed case #TaG #tag #TAG", vec!["tag"]),
            (
                "#tag1, #tag2, #tag3 with commas",
                vec!["tag1", "tag2", "tag3"],
            ),
            ("Separated by commas #tag1,#tag2", vec!["tag1", "tag2"]),
            ("Separated by periods #tag1.#tag2", vec!["tag1", "tag2"]),
            ("Separated by semicolons #tag1;#tag2", vec!["tag1", "tag2"]),
        ];

        for (input, expected) in test_cases {
            let result = NewPost::extract_hashtags(input);
            let expected: HashSet<String> = expected.into_iter().map(String::from).collect();
            assert_eq!(result, expected, "Failed for input: {}", input);
        }
    }
}
