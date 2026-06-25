//! Time blocks and the overlap layout used to place them side-by-side.
//!
//! A [`Block`] is a span of time with a title and color. Eventually these come
//! from NIP-52 time-based calendar events (kind `31923`); for now [`demo`]
//! seeds a sample day so we have something to render.

use chrono::{DateTime, Local, TimeZone};
use egui::Color32;

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

// Tailwind-ish 500 palette; white title text reads on all of these.
const SKY: Color32 = Color32::from_rgb(0x0E, 0xA5, 0xE9);
const VIOLET: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
const EMERALD: Color32 = Color32::from_rgb(0x10, 0xB9, 0x81);
const AMBER: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);
const ROSE: Color32 = Color32::from_rgb(0xF4, 0x3F, 0x5E);
const INDIGO: Color32 = Color32::from_rgb(0x63, 0x66, 0xF1);

/// Sample blocks for the day containing `now`, including a couple of
/// intentional overlaps to exercise the lane layout.
///
/// TODO: replace with NIP-52 calendar reads (see the "read NIP-52 calendar
/// events from nostrdb" card).
pub(crate) fn demo(now: DateTime<Local>) -> Vec<Block> {
    let day = now.date_naive();
    let at = |h: u32, m: u32| {
        Local
            .from_local_datetime(&day.and_hms_opt(h, m, 0).unwrap())
            .single()
            .unwrap()
    };

    vec![
        Block::new("Morning routine", at(7, 0), at(8, 30), AMBER),
        Block::new("Deep work", at(9, 0), at(12, 0), SKY),
        Block::new("Standup", at(9, 30), at(10, 0), ROSE),
        Block::new("Lunch", at(12, 0), at(13, 0), EMERALD),
        Block::new("Design review", at(13, 0), at(14, 30), VIOLET),
        Block::new("Emails", at(13, 30), at(14, 0), ROSE),
        Block::new("Gym", at(18, 0), at(19, 0), INDIGO),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(h0: u32, m0: u32, h1: u32, m1: u32) -> Block {
        let at = |h, m| Local.with_ymd_and_hms(2026, 6, 25, h, m, 0).unwrap();
        Block::new("x", at(h0, m0), at(h1, m1), SKY)
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
