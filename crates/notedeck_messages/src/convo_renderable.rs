use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use nostrdb::NoteKey;

use crate::cache::NotePkg;

pub struct ConversationRenderable {
    items: Vec<ConversationItem>,
}

#[derive(Debug)]
pub enum ConversationItem {
    Date(NaiveDate),
    Message { msg_type: MessageType, key: NoteKey },
}

#[derive(PartialEq, Copy, Clone, Debug)]
pub enum MessageType {
    Standalone,
    FirstInSeries,
    MiddleInSeries,
    LastInSeries,
}

impl ConversationRenderable {
    pub fn new(ordered_msgs: &[NotePkg]) -> Self {
        Self {
            items: generate_conversation_renderable(ordered_msgs),
        }
    }

    pub fn get(&self, index: usize) -> Option<&ConversationItem> {
        self.items.get(index)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ordered_msgs ordered from newest to oldest. Need it ordered oldest to newest
fn generate_conversation_renderable(ordered_msgs: &[NotePkg]) -> Vec<ConversationItem> {
    let mut items = Vec::with_capacity(ordered_msgs.len());
    let midnight_anchor = local_midnight_anchor();

    let mut iter = ordered_msgs.iter().rev();

    let Some(mut prev) = iter.next() else {
        return vec![];
    };

    let mut prev_anchor_dist = days_since_anchor(&midnight_anchor, prev.note_ref.created_at);

    items.push(ConversationItem::Date(prev_anchor_dist.dt));

    let Some(mut cur) = iter.next() else {
        items.push(ConversationItem::Message {
            msg_type: cur_message_type(
                None,
                AnchoredPkg {
                    pkg: prev,
                    distance: &prev_anchor_dist,
                },
                None,
            ),
            key: prev.note_ref.key,
        });
        return items;
    };

    let mut cur_anchor_dist = days_since_anchor(&midnight_anchor, cur.note_ref.created_at);
    items.push(ConversationItem::Message {
        msg_type: cur_message_type(
            None,
            AnchoredPkg {
                pkg: prev,
                distance: &prev_anchor_dist,
            },
            Some(AnchoredPkg {
                pkg: cur,
                distance: &cur_anchor_dist,
            }),
        ),
        key: prev.note_ref.key,
    });

    for next in iter {
        if prev_anchor_dist.days_from_anchor != cur_anchor_dist.days_from_anchor {
            items.push(ConversationItem::Date(cur_anchor_dist.dt));
        }

        let next_anchor_dist = days_since_anchor(&midnight_anchor, next.note_ref.created_at);
        items.push(ConversationItem::Message {
            msg_type: cur_message_type(
                Some(AnchoredPkg {
                    pkg: prev,
                    distance: &prev_anchor_dist,
                }),
                AnchoredPkg {
                    pkg: cur,
                    distance: &cur_anchor_dist,
                },
                Some(AnchoredPkg {
                    pkg: next,
                    distance: &next_anchor_dist,
                }),
            ),
            key: cur.note_ref.key,
        });

        prev = cur;
        prev_anchor_dist = cur_anchor_dist;
        cur = next;
        cur_anchor_dist = next_anchor_dist;
    }

    if prev_anchor_dist.days_from_anchor != cur_anchor_dist.days_from_anchor {
        items.push(ConversationItem::Date(cur_anchor_dist.dt));
    }

    items.push(ConversationItem::Message {
        msg_type: cur_message_type(
            Some(AnchoredPkg {
                pkg: prev,
                distance: &prev_anchor_dist,
            }),
            AnchoredPkg {
                pkg: cur,
                distance: &cur_anchor_dist,
            },
            None,
        ),
        key: cur.note_ref.key,
    });

    items
}

struct AnchoredPkg<'a> {
    pkg: &'a NotePkg,
    distance: &'a AnchorDistance,
}

static GROUPING_SECS: u64 = 60;
fn cur_message_type(
    prev: Option<AnchoredPkg>,
    cur: AnchoredPkg,
    next: Option<AnchoredPkg>,
) -> MessageType {
    let prev_link = prev.as_ref().is_some_and(|p| series_between(&cur, p));
    let next_link = next.as_ref().is_some_and(|n| series_between(&cur, n));

    match (prev_link, next_link) {
        (false, false) => MessageType::Standalone,
        (false, true) => MessageType::FirstInSeries,
        (true, false) => MessageType::LastInSeries,
        (true, true) => MessageType::MiddleInSeries,
    }
}

fn series_between(a: &AnchoredPkg, b: &AnchoredPkg) -> bool {
    a.distance.days_from_anchor == b.distance.days_from_anchor
        && a.pkg.author == b.pkg.author
        && a.distance.unix_ts.abs_diff(b.distance.unix_ts) < GROUPING_SECS
}

fn local_midnight_anchor() -> NaiveDateTime {
    let epoch_utc = DateTime::<Utc>::UNIX_EPOCH;
    let epoch_local = epoch_utc.with_timezone(&Local);

    epoch_local.date_naive().and_hms_opt(0, 0, 0).unwrap()
}

fn days_since_anchor(anchor: &NaiveDateTime, timestamp: u64) -> AnchorDistance {
    let dt = DateTime::from_timestamp(timestamp as i64, 0)
        .unwrap()
        .with_timezone(&Local)
        .naive_local();

    AnchorDistance {
        days_from_anchor: anchor.signed_duration_since(dt).num_days() as u64,
        dt: dt.date(),
        unix_ts: timestamp,
    }
}

struct AnchorDistance {
    days_from_anchor: u64, // distance in anchor, in days
    unix_ts: u64,
    dt: NaiveDate,
}
