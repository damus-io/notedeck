use egui::{
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditOutput,
    TextBuffer, TextEdit, TextFormat,
};
use enostr::{FullKeypair, Pubkey};
use nostrdb::{Note, NoteBuilder, NoteReply};
use std::{
    any::TypeId,
    collections::{BTreeMap, HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
};
use tracing::error;

use crate::media_upload::Nip94Event;

pub struct NewPost {
    pub content: String,
    pub account: FullKeypair,
    pub media: Vec<Nip94Event>,
    pub mentions: Vec<Pubkey>,
}

fn client_variant() -> &'static str {
    #[cfg(target_os = "android")]
    {
        "Damus Android"
    }

    #[cfg(not(target_os = "android"))]
    {
        "Damus Notedeck"
    }
}

fn add_client_tag(builder: NoteBuilder<'_>) -> NoteBuilder<'_> {
    builder
        .start_tag()
        .tag_str("client")
        .tag_str(client_variant())
}

impl NewPost {
    pub fn new(
        content: String,
        account: enostr::FullKeypair,
        media: Vec<Nip94Event>,
        mentions: Vec<Pubkey>,
    ) -> Self {
        NewPost {
            content,
            account,
            media,
            mentions,
        }
    }

    /// creates a NoteBuilder with all the shared data between note, reply & quote reply
    fn builder_with_shared_tags<'a>(&self, mut content: String) -> NoteBuilder<'a> {
        append_urls(&mut content, &self.media);

        let mut builder = NoteBuilder::new().kind(1).content(&content);
        builder = add_client_tag(builder);

        for hashtag in Self::extract_hashtags(&self.content) {
            builder = builder.start_tag().tag_str("t").tag_str(&hashtag);
        }

        if !self.media.is_empty() {
            builder = add_imeta_tags(builder, &self.media);
        }

        if !self.mentions.is_empty() {
            builder = add_mention_tags(builder, &self.mentions);
        }

        builder
    }

    pub fn to_note(&self, seckey: &[u8; 32]) -> Note<'_> {
        let builder = self.builder_with_shared_tags(self.content.clone());

        builder.sign(seckey).build().expect("note should be ok")
    }

    pub fn to_reply(&self, seckey: &[u8; 32], replying_to: &Note) -> Note<'_> {
        let mut builder = self.builder_with_shared_tags(self.content.clone());

        let nip10 = NoteReply::new(replying_to.tags());

        builder = if let Some(root) = nip10.root() {
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

    pub fn to_quote(&self, seckey: &[u8; 32], quoting: &Note) -> Note<'_> {
        let new_content = format!(
            "{}\nnostr:{}",
            self.content,
            enostr::NoteId::new(*quoting.id()).to_bech().unwrap()
        );

        let builder = self.builder_with_shared_tags(new_content);

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

fn add_mention_tags<'a>(builder: NoteBuilder<'a>, mentions: &Vec<Pubkey>) -> NoteBuilder<'a> {
    let mut builder = builder;

    for mention in mentions {
        builder = builder.start_tag().tag_str("p").tag_str(&mention.hex());
    }

    builder
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

type MentionKey = usize;

#[derive(Debug, Clone)]
pub struct PostBuffer {
    pub text_buffer: String,
    pub mention_indicator: char,
    pub mentions: HashMap<MentionKey, MentionInfo>,
    mentions_key: MentionKey,
    pub selected_mention: bool,

    // the start index of a mention is inclusive
    pub mention_starts: BTreeMap<usize, MentionKey>, // maps the mention start index with the correct `MentionKey`

    // the end index of a mention is exclusive
    pub mention_ends: BTreeMap<usize, MentionKey>, // maps the mention end index with the correct `MentionKey`
}

impl Default for PostBuffer {
    fn default() -> Self {
        Self {
            mention_indicator: '@',
            mentions_key: 0,
            selected_mention: false,
            text_buffer: Default::default(),
            mentions: Default::default(),
            mention_starts: Default::default(),
            mention_ends: Default::default(),
        }
    }
}

/// New cursor index (indexed by characters) after operation is performed
#[must_use = "must call MentionSelectedResponse::process"]
pub struct MentionSelectedResponse {
    pub next_cursor_index: usize,
}

impl MentionSelectedResponse {
    pub fn process(&self, ctx: &egui::Context, text_edit_output: &TextEditOutput) {
        let text_edit_id = text_edit_output.response.id;
        let Some(mut before_state) = TextEdit::load_state(ctx, text_edit_id) else {
            return;
        };

        let mut new_cursor = text_edit_output
            .galley
            .from_ccursor(CCursor::new(self.next_cursor_index));
        new_cursor.ccursor.prefer_next_row = true;

        before_state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(
                self.next_cursor_index,
            ))));

        ctx.memory_mut(|mem| mem.request_focus(text_edit_id));

        TextEdit::store_state(ctx, text_edit_id, before_state);
    }
}

impl PostBuffer {
    pub fn get_new_mentions_key(&mut self) -> usize {
        let prev = self.mentions_key;
        self.mentions_key += 1;
        prev
    }

    pub fn get_mention(&self, cursor_index: usize) -> Option<MentionIndex<'_>> {
        self.mention_ends
            .range(cursor_index..)
            .next()
            .and_then(|(_, mention_key)| {
                self.mentions
                    .get(mention_key)
                    .filter(|info| {
                        if let MentionType::Finalized(_) = info.mention_type {
                            // should exclude the last character if we're finalized
                            info.start_index <= cursor_index && cursor_index < info.end_index
                        } else {
                            info.start_index <= cursor_index && cursor_index <= info.end_index
                        }
                    })
                    .map(|info| MentionIndex {
                        index: *mention_key,
                        info,
                    })
            })
    }

    pub fn get_mention_string<'a>(&'a self, mention_key: &MentionIndex<'a>) -> &'a str {
        self.text_buffer
            .char_range(mention_key.info.start_index + 1..mention_key.info.end_index)
        // don't include the delim
    }

    pub fn select_full_mention(&mut self, mention_key: usize, pk: Pubkey) {
        if let Some(info) = self.mentions.get_mut(&mention_key) {
            info.mention_type = MentionType::Finalized(pk);
            self.selected_mention = true;
        } else {
            error!("Error selecting mention for index: {mention_key}. Have the following mentions: {:?}", self.mentions);
        }
    }

    pub fn select_mention_and_replace_name(
        &mut self,
        mention_key: usize,
        full_name: &str,
        pk: Pubkey,
    ) -> Option<MentionSelectedResponse> {
        let Some(info) = self.mentions.get(&mention_key) else {
            error!("Error selecting mention for index: {mention_key}. Have the following mentions: {:?}", self.mentions);
            return None;
        };
        let text_start_index = info.start_index + 1; // increment by one to exclude the mention indicator, '@'
        self.delete_char_range(text_start_index..info.end_index);
        let text_chars_inserted = self.insert_text(full_name, text_start_index);
        self.select_full_mention(mention_key, pk);

        let space_chars_inserted = self.insert_text(" ", text_start_index + text_chars_inserted);

        Some(MentionSelectedResponse {
            next_cursor_index: text_start_index + text_chars_inserted + space_chars_inserted,
        })
    }

    pub fn delete_mention(&mut self, mention_key: usize) {
        if let Some(mention_info) = self.mentions.get(&mention_key) {
            self.mention_starts.remove(&mention_info.start_index);
            self.mention_ends.remove(&mention_info.end_index);
            self.mentions.remove(&mention_key);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text_buffer.is_empty()
    }

    pub fn output(&self) -> PostOutput {
        let mut out = self.text_buffer.clone();
        let mut mentions = Vec::new();
        for (cur_end_ind, mention_ind) in self.mention_ends.iter().rev() {
            if let Some(info) = self.mentions.get(mention_ind) {
                if let MentionType::Finalized(pk) = info.mention_type {
                    if let Some(bech) = pk.npub() {
                        if let Some(byte_range) =
                            char_indices_to_byte(&out, info.start_index..*cur_end_ind)
                        {
                            out.replace_range(byte_range, &format!("nostr:{bech}"));
                            mentions.push(pk);
                        }
                    }
                }
            }
        }
        mentions.reverse();

        PostOutput {
            text: out,
            mentions,
        }
    }

    pub fn to_layout_job(&self, ui: &egui::Ui) -> LayoutJob {
        let mut job = LayoutJob::default();
        let colored_fmt = default_text_format_colored(ui, notedeck_ui::colors::PINK);

        let mut prev_text_char_index = 0;
        let mut prev_text_byte_index = 0;
        for (start_char_index, mention_ind) in &self.mention_starts {
            if let Some(info) = self.mentions.get(mention_ind) {
                if matches!(info.mention_type, MentionType::Finalized(_)) {
                    let end_char_index = info.end_index;

                    let char_indices = prev_text_char_index..*start_char_index;
                    if let Some(byte_indicies) =
                        char_indices_to_byte(&self.text_buffer, char_indices.clone())
                    {
                        if let Some(prev_text) = self.text_buffer.get(byte_indicies.clone()) {
                            job.append(prev_text, 0.0, default_text_format(ui));
                            prev_text_char_index = *start_char_index;
                            prev_text_byte_index = byte_indicies.end;
                        }
                    }

                    let char_indices = *start_char_index..end_char_index;
                    if let Some(byte_indicies) =
                        char_indices_to_byte(&self.text_buffer, char_indices.clone())
                    {
                        if let Some(cur_text) = self.text_buffer.get(byte_indicies.clone()) {
                            job.append(cur_text, 0.0, colored_fmt.clone());
                            prev_text_char_index = end_char_index;
                            prev_text_byte_index = byte_indicies.end;
                        }
                    }
                }
            }
        }

        if prev_text_byte_index < self.text_buffer.len() {
            if let Some(cur_text) = self.text_buffer.get(prev_text_byte_index..) {
                job.append(cur_text, 0.0, default_text_format(ui));
            } else {
                error!(
                    "could not retrieve substring from [{} to {}) in PostBuffer::text_buffer",
                    prev_text_byte_index,
                    self.text_buffer.len()
                );
            }
        }

        job
    }

    pub fn need_new_layout(&self, cache: Option<&(String, LayoutJob)>) -> bool {
        if let Some((text, _)) = cache {
            if self.selected_mention {
                return true;
            }

            self.text_buffer != *text
        } else {
            true
        }
    }
}

fn char_indices_to_byte(text: &str, char_range: Range<usize>) -> Option<Range<usize>> {
    let mut char_indices = text.char_indices();

    let start = char_indices.nth(char_range.start)?.0;
    let end = if char_range.end < text.chars().count() {
        char_indices.nth(char_range.end - char_range.start - 1)?.0
    } else {
        text.len()
    };

    Some(start..end)
}

pub fn downcast_post_buffer(buffer: &dyn TextBuffer) -> Option<&PostBuffer> {
    let mut hasher = DefaultHasher::new();
    TypeId::of::<PostBuffer>().hash(&mut hasher);
    let post_id = hasher.finish() as usize;

    if buffer.type_id() == post_id {
        unsafe { Some(&*(buffer as *const dyn TextBuffer as *const PostBuffer)) }
    } else {
        None
    }
}

fn default_text_format(ui: &egui::Ui) -> TextFormat {
    default_text_format_colored(
        ui,
        ui.visuals()
            .override_text_color
            .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color()),
    )
}

fn default_text_format_colored(ui: &egui::Ui, color: egui::Color32) -> TextFormat {
    TextFormat::simple(egui::FontSelection::default().resolve(ui.style()), color)
}

pub struct PostOutput {
    pub text: String,
    pub mentions: Vec<Pubkey>,
}

#[derive(Debug)]
pub struct MentionIndex<'a> {
    pub index: usize,
    pub info: &'a MentionInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MentionType {
    Pending,
    Finalized(Pubkey),
}

impl TextBuffer for PostBuffer {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text_buffer.as_str()
    }

    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        if text.is_empty() {
            return 0;
        }
        let text_num_chars = text.chars().count();
        self.text_buffer.insert_text(text, char_index);

        // the text was inserted before or inside these mentions. We need to at least move their ends
        let pending_ends_to_update: Vec<usize> = self
            .mention_ends
            .range(char_index..)
            .filter(|(k, v)| {
                let is_last = **k == char_index;
                let is_finalized = if let Some(info) = self.mentions.get(*v) {
                    matches!(info.mention_type, MentionType::Finalized(_))
                } else {
                    false
                };
                !(is_last && is_finalized)
            })
            .map(|(&k, _)| k)
            .collect();

        let mut break_mentions = Vec::new();
        for cur_end in pending_ends_to_update {
            let mention_key = if let Some(mention_key) = self.mention_ends.get(&cur_end) {
                *mention_key
            } else {
                continue;
            };

            self.mention_ends.remove(&cur_end);

            let new_end = cur_end + text_num_chars;
            self.mention_ends.insert(new_end, mention_key);
            // replaced the current end with the new value

            if let Some(mention_info) = self.mentions.get_mut(&mention_key) {
                if mention_info.start_index >= char_index {
                    // the text is being inserted before this mention. move the start index as well
                    self.mention_starts.remove(&mention_info.start_index);
                    let new_start = mention_info.start_index + text_num_chars;
                    self.mention_starts.insert(new_start, mention_key);
                    mention_info.start_index = new_start;
                } else {
                    if char_index == mention_info.end_index
                        && first_is_desired_char(&self.text_buffer, text, char_index, ' ')
                    {
                        // if the user wrote a double space at the end of the mention, break it
                        break_mentions.push(mention_key);
                    }

                    // text is being inserted inside this mention. Make sure it is in the pending state
                    mention_info.mention_type = MentionType::Pending;
                }

                mention_info.end_index = new_end;
            } else {
                error!("Could not find mention at index {}", mention_key);
            }
        }

        for mention_key in break_mentions {
            self.delete_mention(mention_key);
        }

        if first_is_desired_char(&self.text_buffer, text, char_index, self.mention_indicator) {
            // if a mention already exists where we're inserting the delim, remove it
            let to_remove = self.get_mention(char_index).map(|old_mention| {
                (
                    old_mention.index,
                    old_mention.info.start_index..old_mention.info.end_index,
                )
            });

            if let Some((key, range)) = to_remove {
                self.mention_ends.remove(&range.end);
                self.mention_starts.remove(&range.start);
                self.mentions.remove(&key);
            }

            let start_index = char_index;
            let end_index = char_index + text_num_chars;
            let mention_key = self.get_new_mentions_key();
            self.mentions.insert(
                mention_key,
                MentionInfo {
                    start_index,
                    end_index,
                    mention_type: MentionType::Pending,
                },
            );
            self.mention_starts.insert(start_index, mention_key);
            self.mention_ends.insert(end_index, mention_key);
        }

        text_num_chars
    }

    fn delete_char_range(&mut self, char_range: Range<usize>) {
        let deletion_num_chars = char_range.len();
        let Range {
            start: deletion_start,
            end: deletion_end,
        } = char_range;

        self.text_buffer.delete_char_range(char_range);

        // these mentions will be affected by the deletion
        let ends_to_update: Vec<usize> = self
            .mention_ends
            .range(deletion_start..)
            .map(|(&k, _)| k)
            .collect();

        for cur_mention_end in ends_to_update {
            let mention_key = match &self.mention_ends.get(&cur_mention_end) {
                Some(ind) => **ind,
                None => continue,
            };
            let cur_mention_start = match self.mentions.get(&mention_key) {
                Some(i) => i.start_index,
                None => {
                    error!("Could not find mention at index {}", mention_key);
                    continue;
                }
            };

            if cur_mention_end <= deletion_start {
                // nothing happens to this mention
                continue;
            }

            let status = if cur_mention_start >= deletion_start {
                if cur_mention_start >= deletion_end {
                    // mention falls after the range
                    // need to shift both start and end

                    DeletionStatus::ShiftStartAndEnd(
                        cur_mention_start - deletion_num_chars,
                        cur_mention_end - deletion_num_chars,
                    )
                } else {
                    // fully delete mention

                    DeletionStatus::FullyRemove
                }
            } else if cur_mention_end > deletion_end {
                // inner partial delete

                DeletionStatus::ShiftEnd(cur_mention_end - deletion_num_chars)
            } else {
                // outer partial delete

                DeletionStatus::ShiftEnd(deletion_start)
            };

            match status {
                DeletionStatus::FullyRemove => {
                    self.mention_starts.remove(&cur_mention_start);
                    self.mention_ends.remove(&cur_mention_end);
                    self.mentions.remove(&mention_key);
                }
                DeletionStatus::ShiftEnd(new_end)
                | DeletionStatus::ShiftStartAndEnd(_, new_end) => {
                    let mention_info = match self.mentions.get_mut(&mention_key) {
                        Some(i) => i,
                        None => {
                            error!("Could not find mention at index {}", mention_key);
                            continue;
                        }
                    };

                    self.mention_ends.remove(&cur_mention_end);
                    self.mention_ends.insert(new_end, mention_key);
                    mention_info.end_index = new_end;

                    if let DeletionStatus::ShiftStartAndEnd(new_start, _) = status {
                        self.mention_starts.remove(&cur_mention_start);
                        self.mention_starts.insert(new_start, mention_key);
                        mention_info.start_index = new_start;
                    }

                    if let DeletionStatus::ShiftEnd(_) = status {
                        mention_info.mention_type = MentionType::Pending;
                    }
                }
            }
        }
    }

    fn type_id(&self) -> usize {
        let mut hasher = DefaultHasher::new();
        TypeId::of::<PostBuffer>().hash(&mut hasher);
        hasher.finish() as usize
    }
}

fn first_is_desired_char(
    full_text: &str,
    new_text: &str,
    new_text_index: usize,
    desired: char,
) -> bool {
    new_text.chars().next().is_some_and(|c| {
        c == desired
            && (new_text_index == 0 || full_text.chars().nth(new_text_index - 1) == Some(' '))
    })
}

#[derive(Debug)]
enum DeletionStatus {
    FullyRemove,
    ShiftEnd(usize),
    ShiftStartAndEnd(usize, usize),
}

#[derive(Debug, PartialEq, Clone)]
pub struct MentionInfo {
    pub start_index: usize,
    pub end_index: usize,
    pub mention_type: MentionType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    impl MentionInfo {
        pub fn bounds(&self) -> Range<usize> {
            self.start_index..self.end_index
        }
    }

    const JB55: fn() -> Pubkey = || {
        Pubkey::from_hex("32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245")
            .unwrap()
    };
    const KK: fn() -> Pubkey = || {
        Pubkey::from_hex("4a0510f26880d40e432f4865cb5714d9d3c200ca6ebb16b418ae6c555f574967")
            .unwrap()
    };

    #[derive(PartialEq, Clone, Debug)]
    struct MentionExample {
        text: String,
        mention1: Option<MentionInfo>,
        mention2: Option<MentionInfo>,
        mention3: Option<MentionInfo>,
        mention4: Option<MentionInfo>,
    }

    fn apply_mention_example(buf: &mut PostBuffer) -> MentionExample {
        buf.insert_text("test ", 0);
        buf.insert_text("@jb55", 5);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test ", 10);
        buf.insert_text("@vrod", 16);
        buf.select_full_mention(1, JB55());
        buf.insert_text(" test ", 21);
        buf.insert_text("@elsat", 27);
        buf.select_full_mention(2, JB55());
        buf.insert_text(" test ", 33);
        buf.insert_text("@kernelkind", 39);
        buf.select_full_mention(3, KK());
        buf.insert_text(" test", 50);

        let mention1_bounds = 5..10;
        let mention2_bounds = 16..21;
        let mention3_bounds = 27..33;
        let mention4_bounds = 39..50;

        let text = "test @jb55 test @vrod test @elsat test @kernelkind test";

        assert_eq!(buf.as_str(), text);
        assert_eq!(buf.mentions.len(), 4);

        let mention1 = buf.mentions.get(&0).unwrap();
        assert_eq!(mention1.bounds(), mention1_bounds);
        assert_eq!(mention1.mention_type, MentionType::Finalized(JB55()));
        let mention2 = buf.mentions.get(&1).unwrap();
        assert_eq!(mention2.bounds(), mention2_bounds);
        assert_eq!(mention2.mention_type, MentionType::Finalized(JB55()));
        let mention3 = buf.mentions.get(&2).unwrap();
        assert_eq!(mention3.bounds(), mention3_bounds);
        assert_eq!(mention3.mention_type, MentionType::Finalized(JB55()));
        let mention4 = buf.mentions.get(&3).unwrap();
        assert_eq!(mention4.bounds(), mention4_bounds);
        assert_eq!(mention4.mention_type, MentionType::Finalized(KK()));

        let text = text.to_owned();
        MentionExample {
            text,
            mention1: Some(mention1.clone()),
            mention2: Some(mention2.clone()),
            mention3: Some(mention3.clone()),
            mention4: Some(mention4.clone()),
        }
    }

    impl PostBuffer {
        fn to_example(&self) -> MentionExample {
            let mention1 = self.mentions.get(&0).cloned();
            let mention2 = self.mentions.get(&1).cloned();
            let mention3 = self.mentions.get(&2).cloned();
            let mention4 = self.mentions.get(&3).cloned();

            MentionExample {
                text: self.text_buffer.clone(),
                mention1,
                mention2,
                mention3,
                mention4,
            }
        }
    }

    impl MentionInfo {
        fn shifted(mut self, offset: usize) -> Self {
            self.end_index -= offset;
            self.start_index -= offset;

            self
        }
    }

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

    #[test]
    fn test_insert_single_mention() {
        let mut buf = PostBuffer::default();
        buf.insert_text("test ", 0);
        buf.insert_text("@", 5);
        assert!(buf.get_mention(5).is_some());
        buf.insert_text("jb55", 6);
        assert_eq!(buf.as_str(), "test @jb55");
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 5..10);

        buf.select_full_mention(0, JB55());

        assert_eq!(
            buf.mentions.get(&0).unwrap().mention_type,
            MentionType::Finalized(JB55())
        );
    }

    #[test]
    fn test_insert_mention_with_space() {
        let mut buf = PostBuffer::default();
        buf.insert_text("@", 0);
        buf.insert_text("jb", 1);
        buf.insert_text("55", 3);
        assert!(buf.get_mention(1).is_some());
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 0..5);
        buf.insert_text(" test", 5);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 0..10);
        assert_eq!(buf.as_str(), "@jb55 test");

        buf.select_full_mention(0, JB55());

        assert_eq!(
            buf.mentions.get(&0).unwrap().mention_type,
            MentionType::Finalized(JB55())
        );
    }

    #[test]
    fn test_insert_mention_with_emojis() {
        let mut buf = PostBuffer::default();
        buf.insert_text("test ", 0);
        buf.insert_text("@test😀 🏴‍☠️ :D", 5);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test", 19);

        assert_eq!(buf.as_str(), "test @test😀 🏴‍☠️ :D test");
        let mention = buf.mentions.get(&0).unwrap();
        assert_eq!(
            *mention,
            MentionInfo {
                start_index: 5,
                end_index: 19,
                mention_type: MentionType::Finalized(JB55())
            }
        );
    }

    #[test]
    fn test_insert_partial_to_full() {
        let mut buf = PostBuffer::default();
        buf.insert_text("@jb", 0);
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 0..3);
        buf.select_mention_and_replace_name(0, "jb55", JB55());
        assert_eq!(buf.as_str(), "@jb55 ");

        buf.insert_text("test", 6);
        assert_eq!(buf.as_str(), "@jb55 test");

        assert_eq!(buf.mentions.len(), 1);
        let mention = buf.mentions.get(&0).unwrap();
        assert_eq!(mention.bounds(), 0..5);
        assert_eq!(mention.mention_type, MentionType::Finalized(JB55()));
    }

    #[test]
    fn test_insert_mention_after_text() {
        let mut buf = PostBuffer::default();
        buf.insert_text("test text here", 0);
        buf.insert_text("@jb55", 4);

        assert!(buf.mentions.is_empty());
    }

    #[test]
    fn test_insert_mention_with_space_after_text() {
        let mut buf = PostBuffer::default();
        buf.insert_text("test  text here", 0);
        buf.insert_text("@jb55", 5);

        assert!(buf.get_mention(5).is_some());
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 5..10);
        assert_eq!("test @jb55 text here", buf.as_str());

        buf.select_full_mention(0, JB55());

        assert_eq!(
            buf.mentions.get(&0).unwrap().mention_type,
            MentionType::Finalized(JB55())
        );
    }

    #[test]
    fn test_insert_mention_then_text() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());

        buf.insert_text(" test", 5);
        assert_eq!(buf.as_str(), "@jb55 test");
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 0..5);
        assert!(buf.get_mention(6).is_none());
    }

    #[test]
    fn test_insert_two_mentions() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test ", 5);
        buf.insert_text("@kernelkind", 11);
        buf.select_full_mention(1, KK());
        buf.insert_text(" test", 22);

        assert_eq!(buf.as_str(), "@jb55 test @kernelkind test");
        assert_eq!(buf.mentions.len(), 2);
        assert_eq!(buf.mentions.get(&0).unwrap().bounds(), 0..5);
        assert_eq!(buf.mentions.get(&1).unwrap().bounds(), 11..22);
    }

    #[test]
    fn test_insert_into_mention() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test", 5);

        assert_eq!(buf.mentions.len(), 1);
        let mention = buf.mentions.get(&0).unwrap();
        assert_eq!(mention.bounds(), 0..5);
        assert_eq!(mention.mention_type, MentionType::Finalized(JB55()));

        buf.insert_text("oops", 2);
        assert_eq!(buf.as_str(), "@joopsb55 test");
        assert_eq!(buf.mentions.len(), 1);
        let mention = buf.mentions.get(&0).unwrap();
        assert_eq!(mention.bounds(), 0..9);
        assert_eq!(mention.mention_type, MentionType::Pending);
    }

    #[test]
    fn test_insert_mention_inside_mention() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test", 5);

        assert_eq!(buf.mentions.len(), 1);
        let mention = buf.mentions.get(&0).unwrap();
        assert_eq!(mention.bounds(), 0..5);
        assert_eq!(mention.mention_type, MentionType::Finalized(JB55()));

        buf.insert_text(" ", 3);
        buf.insert_text("@oops", 4);
        assert_eq!(buf.as_str(), "@jb @oops55 test");
        assert_eq!(buf.mentions.len(), 1);
        assert_eq!(buf.mention_ends.len(), 1);
        assert_eq!(buf.mention_starts.len(), 1);
        let mention = buf.mentions.get(&1).unwrap();
        assert_eq!(mention.bounds(), 4..9);
        assert_eq!(mention.mention_type, MentionType::Pending);
    }

    #[test]
    fn test_delete_before_mention() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        let range = 1..5;
        let len = range.len();
        buf.delete_char_range(range);

        assert_eq!(
            MentionExample {
                text: "t@jb55 test @vrod test @elsat test @kernelkind test".to_owned(),
                mention1: Some(before.mention1.clone().unwrap().shifted(len)),
                mention2: Some(before.mention2.clone().unwrap().shifted(len)),
                mention3: Some(before.mention3.clone().unwrap().shifted(len)),
                mention4: Some(before.mention4.clone().unwrap().shifted(len)),
            },
            buf.to_example(),
        );
    }

    #[test]
    fn test_delete_after_mention() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        let range = 11..16;
        let len = range.len();
        buf.delete_char_range(range);

        assert_eq!(
            MentionExample {
                text: "test @jb55 @vrod test @elsat test @kernelkind test".to_owned(),
                mention2: Some(before.mention2.clone().unwrap().shifted(len)),
                mention3: Some(before.mention3.clone().unwrap().shifted(len)),
                mention4: Some(before.mention4.clone().unwrap().shifted(len)),
                ..before.clone()
            },
            buf.to_example(),
        );
    }

    #[test]
    fn test_delete_mention_partial_inner() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        let range = 17..20;
        let len = range.len();
        buf.delete_char_range(range);

        assert_eq!(
            MentionExample {
                text: "test @jb55 test @d test @elsat test @kernelkind test".to_owned(),
                mention2: Some(MentionInfo {
                    start_index: 16,
                    end_index: 18,
                    mention_type: MentionType::Pending,
                }),
                mention3: Some(before.mention3.clone().unwrap().shifted(len)),
                mention4: Some(before.mention4.clone().unwrap().shifted(len)),
                ..before.clone()
            },
            buf.to_example(),
        );
    }

    #[test]
    fn test_delete_mention_partial_outer() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        let range = 17..27;
        let len = range.len();
        buf.delete_char_range(range);

        assert_eq!(
            MentionExample {
                text: "test @jb55 test @@elsat test @kernelkind test".to_owned(),
                mention2: Some(MentionInfo {
                    start_index: 16,
                    end_index: 17,
                    mention_type: MentionType::Pending
                }),
                mention3: Some(before.mention3.clone().unwrap().shifted(len)),
                mention4: Some(before.mention4.clone().unwrap().shifted(len)),
                ..before.clone()
            },
            buf.to_example(),
        );
    }

    #[test]
    fn test_delete_mention_partial_and_full() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        buf.delete_char_range(17..28);

        assert_eq!(
            MentionExample {
                text: "test @jb55 test @elsat test @kernelkind test".to_owned(),
                mention2: Some(MentionInfo {
                    end_index: 17,
                    mention_type: MentionType::Pending,
                    ..before.mention2.clone().unwrap()
                }),
                mention3: None,
                mention4: Some(MentionInfo {
                    start_index: 28,
                    end_index: 39,
                    ..before.mention4.clone().unwrap()
                }),
                ..before.clone()
            },
            buf.to_example()
        )
    }

    #[test]
    fn test_delete_mention_full_one() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        let range = 10..26;
        let len = range.len();
        buf.delete_char_range(range);

        assert_eq!(
            MentionExample {
                text: "test @jb55 @elsat test @kernelkind test".to_owned(),
                mention2: None,
                mention3: Some(before.mention3.clone().unwrap().shifted(len)),
                mention4: Some(before.mention4.clone().unwrap().shifted(len)),
                ..before.clone()
            },
            buf.to_example()
        );
    }

    #[test]
    fn test_delete_mention_full_two() {
        let mut buf = PostBuffer::default();
        let before = apply_mention_example(&mut buf);

        buf.delete_char_range(11..28);

        assert_eq!(
            MentionExample {
                text: "test @jb55 elsat test @kernelkind test".to_owned(),
                mention2: None,
                mention3: None,
                mention4: Some(MentionInfo {
                    start_index: 22,
                    end_index: 33,
                    ..before.mention4.clone().unwrap()
                }),
                ..before.clone()
            },
            buf.to_example()
        )
    }

    #[test]
    fn test_two_then_one_between() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb", 0);
        buf.select_mention_and_replace_name(0, "jb55", JB55());
        buf.insert_text("test ", 6);
        assert_eq!(buf.as_str(), "@jb55 test ");
        buf.insert_text("@kernel", 11);
        buf.select_mention_and_replace_name(1, "KernelKind", KK());
        assert_eq!(buf.as_str(), "@jb55 test @KernelKind ");

        buf.insert_text("test", 23);
        assert_eq!(buf.as_str(), "@jb55 test @KernelKind test");

        assert_eq!(buf.mentions.len(), 2);

        buf.insert_text("@els", 6);
        assert_eq!(buf.as_str(), "@jb55 @elstest @KernelKind test");

        assert_eq!(buf.mentions.len(), 3);
        assert_eq!(buf.mentions.get(&2).unwrap().bounds(), 6..10);
        buf.select_mention_and_replace_name(2, "elsat", JB55());
        assert_eq!(buf.as_str(), "@jb55 @elsat test @KernelKind test");

        let jb_mention = buf.mentions.get(&0).unwrap();
        let kk_mention = buf.mentions.get(&1).unwrap();
        let el_mention = buf.mentions.get(&2).unwrap();
        assert_eq!(jb_mention.bounds(), 0..5);
        assert_eq!(jb_mention.mention_type, MentionType::Finalized(JB55()));
        assert_eq!(kk_mention.bounds(), 18..29);
        assert_eq!(kk_mention.mention_type, MentionType::Finalized(KK()));
        assert_eq!(el_mention.bounds(), 6..12);
        assert_eq!(el_mention.mention_type, MentionType::Finalized(JB55()));
    }

    #[test]
    fn note_single_mention() {
        let mut buf = PostBuffer::default();
        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());

        let out = buf.output();
        let kp = FullKeypair::generate();
        let post = NewPost::new(out.text, kp.clone(), Vec::new(), out.mentions);
        let note = post.to_note(&kp.pubkey);

        let mut tags_iter = note.tags().iter();
        tags_iter.next(); //ignore the first one, the client tag
        let tag = tags_iter.next().unwrap();
        assert_eq!(tag.count(), 2);
        assert_eq!(tag.get(0).unwrap().str().unwrap(), "p");
        assert_eq!(tag.get(1).unwrap().id().unwrap(), JB55().bytes());
        assert!(tags_iter.next().is_none());
        assert_eq!(
            note.content(),
            "nostr:npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s"
        );
    }

    #[test]
    fn note_two_mentions() {
        let mut buf = PostBuffer::default();

        buf.insert_text("@jb55", 0);
        buf.select_full_mention(0, JB55());
        buf.insert_text(" test ", 5);
        buf.insert_text("@KernelKind", 11);
        buf.select_full_mention(1, KK());
        buf.insert_text(" test", 22);
        assert_eq!(buf.as_str(), "@jb55 test @KernelKind test");

        let out = buf.output();
        let kp = FullKeypair::generate();
        let post = NewPost::new(out.text, kp.clone(), Vec::new(), out.mentions);
        let note = post.to_note(&kp.pubkey);

        let mut tags_iter = note.tags().iter();
        tags_iter.next(); //ignore the first one, the client tag
        let jb_tag = tags_iter.next().unwrap();
        assert_eq!(jb_tag.count(), 2);
        assert_eq!(jb_tag.get(0).unwrap().str().unwrap(), "p");
        assert_eq!(jb_tag.get(1).unwrap().id().unwrap(), JB55().bytes());

        let kk_tag = tags_iter.next().unwrap();
        assert_eq!(kk_tag.count(), 2);
        assert_eq!(kk_tag.get(0).unwrap().str().unwrap(), "p");
        assert_eq!(kk_tag.get(1).unwrap().id().unwrap(), KK().bytes());

        assert!(tags_iter.next().is_none());

        assert_eq!(note.content(), "nostr:npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s test nostr:npub1fgz3pungsr2quse0fpjuk4c5m8fuyqx2d6a3ddqc4ek92h6hf9ns0mjeck test");
    }

    #[test]
    fn note_one_pending() {
        let mut buf = PostBuffer::default();

        buf.insert_text("test ", 0);
        buf.insert_text("@jb55 test", 5);

        let out = buf.output();
        let kp = FullKeypair::generate();
        let post = NewPost::new(out.text, kp.clone(), Vec::new(), out.mentions);
        let note = post.to_note(&kp.pubkey);

        let mut tags_iter = note.tags().iter();
        tags_iter.next(); //ignore the first one, the client tag
        assert!(tags_iter.next().is_none());
        assert_eq!(note.content(), "test @jb55 test");
    }
}
