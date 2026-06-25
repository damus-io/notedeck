//! Time blocks and the overlap layout used to place them side-by-side.
//!
//! A [`Block`] is a span of time with a title and color. Blocks are
//! materialized from NIP-52 calendar events stored in nostrdb — time-based
//! (kind `31923`) and date-based / all-day (kind `31922`).

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use egui::Color32;
use nostrdb::{Filter, Note};

/// NIP-52 date-based (all-day) calendar event.
pub(crate) const KIND_DATE_BASED: u64 = 31922;
/// NIP-52 time-based calendar event — the core timeblocking primitive.
pub(crate) const KIND_TIME_BASED: u64 = 31923;

/// A single time block on the timeline.
#[derive(Clone, Debug)]
pub struct Block {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub color: Color32,
}

impl Block {
    fn new(title: &str, start: DateTime<Local>, end: DateTime<Local>, color: Color32) -> Self {
        Self {
            title: title.to_owned(),
            start,
            end,
            color,
        }
    }
}

/// Where a block sits within its overlap cluster: `(column, columns)`.
///
/// `column` is the 0-based lane index and `columns` is the number of lanes the
/// cluster was split into, so a renderer can size each block to
/// `width / columns` and offset it by `column`.
pub(crate) type Lane = (usize, usize);

/// Assign lanes to `blocks` so overlapping blocks render side-by-side.
///
/// Returns one [`Lane`] per input block, in the original order. Blocks are
/// grouped into clusters of transitively-overlapping spans; within a cluster
/// each block takes the first lane that's free at its start time, and every
/// block in the cluster reports the cluster's total lane count.
pub(crate) fn layout(blocks: &[&Block]) -> Vec<Lane> {
    let n = blocks.len();

    // Process blocks in start order, breaking ties by end time.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        blocks[a]
            .start
            .cmp(&blocks[b].start)
            .then(blocks[a].end.cmp(&blocks[b].end))
    });

    let mut column = vec![0usize; n];
    let mut columns = vec![1usize; n];

    let mut cluster: Vec<usize> = Vec::new();
    let mut lane_end: Vec<DateTime<Local>> = Vec::new();
    let mut cluster_end: Option<DateTime<Local>> = None;

    fn finalize(cluster: &mut Vec<usize>, lanes: usize, columns: &mut [usize]) {
        for &idx in cluster.iter() {
            columns[idx] = lanes.max(1);
        }
        cluster.clear();
    }

    for &i in &order {
        let b = blocks[i];

        // A gap to every active lane closes the current cluster.
        if cluster_end.is_some_and(|ce| b.start >= ce) {
            finalize(&mut cluster, lane_end.len(), &mut columns);
            lane_end.clear();
            cluster_end = None;
        }

        // Reuse the first lane whose previous block has ended, else open one.
        let mut placed = false;
        for (ci, end) in lane_end.iter_mut().enumerate() {
            if *end <= b.start {
                *end = b.end;
                column[i] = ci;
                placed = true;
                break;
            }
        }
        if !placed {
            column[i] = lane_end.len();
            lane_end.push(b.end);
        }

        cluster.push(i);
        cluster_end = Some(cluster_end.map_or(b.end, |e| e.max(b.end)));
    }
    finalize(&mut cluster, lane_end.len(), &mut columns);

    (0..n).map(|i| (column[i], columns[i])).collect()
}

/// nostrdb filters matching every NIP-52 calendar event we render.
pub(crate) fn calendar_filters() -> Vec<Filter> {
    vec![
        Filter::new()
            .kinds([KIND_DATE_BASED, KIND_TIME_BASED])
            .build(),
    ]
}

/// Build a [`Block`] from a NIP-52 calendar note, or `None` if it isn't a
/// calendar event we can place on the timeline (missing/unparseable `start`).
pub(crate) fn from_note(note: &Note) -> Option<Block> {
    let title = tag_value(note, "title")
        .or_else(|| tag_value(note, "name"))
        .filter(|s| !s.is_empty())
        .unwrap_or("(untitled)")
        .to_owned();

    let (start, end) = match note.kind() as u64 {
        KIND_TIME_BASED => {
            let start = parse_unix(tag_value(note, "start")?)?;
            let end = tag_value(note, "end")
                .and_then(parse_unix)
                .filter(|e| *e > start)
                .unwrap_or_else(|| start + Duration::hours(1));
            (start, end)
        }
        KIND_DATE_BASED => {
            let start = parse_date(tag_value(note, "start")?)?;
            // NIP-52 `end` on a date-based event is exclusive; a missing one
            // means a single all-day event.
            let end = tag_value(note, "end")
                .and_then(parse_date)
                .filter(|e| *e > start)
                .unwrap_or_else(|| start + Duration::days(1));
            (start, end)
        }
        _ => return None,
    };

    Some(Block::new(&title, start, end, color_for(note.id())))
}

/// First single-letter-or-named tag value, e.g. `tag_value(note, "start")`.
fn tag_value<'a>(note: &'a Note<'a>, name: &str) -> Option<&'a str> {
    note.tags().iter().find_map(|tag| {
        (tag.count() >= 2 && tag.get_str(0) == Some(name)).then(|| tag.get_str(1))?
    })
}

/// Parse a NIP-52 unix-timestamp string (`start`/`end` on a time-based event).
fn parse_unix(s: &str) -> Option<DateTime<Local>> {
    let secs: i64 = s.parse().ok()?;
    Some(Utc.timestamp_opt(secs, 0).single()?.with_timezone(&Local))
}

/// Parse a NIP-52 `YYYY-MM-DD` date string into local midnight.
fn parse_date(s: &str) -> Option<DateTime<Local>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()
}

// Tailwind-ish 500 palette; white title text reads on all of these. A block's
// color is picked from its event id so the same event stays the same color.
const PALETTE: [Color32; 6] = [
    Color32::from_rgb(0x0E, 0xA5, 0xE9), // sky
    Color32::from_rgb(0x8B, 0x5C, 0xF6), // violet
    Color32::from_rgb(0x10, 0xB9, 0x81), // emerald
    Color32::from_rgb(0xF5, 0x9E, 0x0B), // amber
    Color32::from_rgb(0xF4, 0x3F, 0x5E), // rose
    Color32::from_rgb(0x63, 0x66, 0xF1), // indigo
];

fn color_for(id: &[u8; 32]) -> Color32 {
    PALETTE[id[0] as usize % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(h0: u32, m0: u32, h1: u32, m1: u32) -> Block {
        let at = |h, m| Local.with_ymd_and_hms(2026, 6, 25, h, m, 0).unwrap();
        Block::new("x", at(h0, m0), at(h1, m1), PALETTE[0])
    }

    fn lanes(blocks: &[Block]) -> Vec<Lane> {
        let refs: Vec<&Block> = blocks.iter().collect();
        layout(&refs)
    }

    #[test]
    fn back_to_back_blocks_share_one_lane() {
        let blocks = [blk(9, 0, 10, 0), blk(10, 0, 11, 0)];
        assert_eq!(lanes(&blocks), vec![(0, 1), (0, 1)]);
    }

    #[test]
    fn overlapping_blocks_split_into_lanes() {
        let blocks = [blk(9, 0, 11, 0), blk(10, 0, 12, 0)];
        assert_eq!(lanes(&blocks), vec![(0, 2), (1, 2)]);
    }

    #[test]
    fn freed_lane_is_reused_within_a_cluster() {
        // `a` frees lane 0 at 10:00, which `c` then reuses while `b` keeps
        // the cluster (and its two-lane width) alive.
        let blocks = [blk(9, 0, 10, 0), blk(9, 30, 11, 0), blk(10, 0, 10, 30)];
        assert_eq!(lanes(&blocks), vec![(0, 2), (1, 2), (0, 2)]);
    }

    #[test]
    fn separate_clusters_are_independent() {
        let blocks = [blk(9, 0, 11, 0), blk(10, 0, 12, 0), blk(13, 0, 14, 0)];
        assert_eq!(lanes(&blocks), vec![(0, 2), (1, 2), (0, 1)]);
    }
}
