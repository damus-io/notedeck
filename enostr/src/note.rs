use crate::Event;
use shatter::shard::Shards;

#[derive(Debug, Eq, PartialEq)]
struct RefId(i32);

struct Ref<'a> {
    ref_tag: u8,
    relay_id: Option<&'a str>,
    id: &'a str,
}

impl<'a> RefId {
    fn get_ref(self, tags: &'a Vec<Vec<String>>) -> Option<Ref<'a>> {
        let ind = self.0 as usize;

        if ind > tags.len() - 1 {
            return None;
        }

        let tag = &tags[ind];

        if tag.len() < 2 {
            return None;
        }

        if tag[0].len() != 1 {
            return None;
        }

        let ref_tag = if let Some(rtag) = tag[0].as_bytes().first() {
            *rtag
        } else {
            0
        };

        let id = &tag[1];
        if id.len() != 64 {
            return None;
        }

        let relay_id = if tag[2].len() == 0 {
            None
        } else {
            Some(&*tag[2])
        };

        Some(Ref {
            ref_tag,
            relay_id,
            id,
        })
    }
}

enum MentionType {
    Pubkey,
    Event,
}

struct Mention {
    index: Option<i32>,
    typ: MentionType,
    refid: RefId,
}

enum EventRef {
    Mention(Mention),
    ThreadId(RefId),
    Reply(RefId),
    ReplyToRoot(RefId),
}

struct EventRefs {
    refs: Vec<EventRef>,
}

struct TextNote {
    event: Event,
    shards: Shards,
    refs: EventRefs,
}

struct DM {
    decrypted: Option<String>,
    shards: Shards,
}

enum Note {
    Text(TextNote),
}
