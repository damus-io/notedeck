//! Per-frame registry of the collapsible blocks in a chat transcript.
//!
//! Every collapsible row in [`DaveUi`](super::DaveUi) — a tool's output, a file
//! edit's diff, a subagent's tool list, a responded permission request — stores
//! its expanded flag in `ui.data()` under a content-derived [`egui::Id`]. Those
//! ids only exist *during* the render pass, so a keybinding handler (which runs
//! before layout) has no way to name a block, let alone the "current" one.
//!
//! [`BlockNav`] closes that gap: the render pass calls
//! [`register`](BlockNav::register) as each block draws, recording its id in
//! visual order, and a keybinding indexes into what the previous frame
//! registered. The storage scheme itself is untouched, so a mouse click still
//! toggles a block exactly as it did before.

/// One collapsible block, as registered by the render pass.
#[derive(Clone, Copy)]
struct BlockRef {
    /// The `egui::Id` its expanded bool is stored under in `ui.data()`.
    id: egui::Id,
    /// What this site defaults to when nothing is stored yet.
    default_open: bool,
    /// The chat row it draws in, as [`enter_row`](BlockNav::enter_row) or
    /// [`enter_fold`](BlockNav::enter_fold) last named it.
    row: Option<usize>,
}

/// Where the block cursor is, kept for the jump list (vim's Ctrl-O): its
/// position and the block's id, so a restore lands on the same block even
/// if blocks streamed in above it since.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockSpot {
    /// The cursor's position in registration order.
    pub index: usize,
    /// The id of the block it was on.
    pub id: egui::Id,
}

/// What a collapsible row learns about itself the moment it registers: how to
/// draw, whether the keyboard cursor is on it, and whether this frame should
/// bring it into view.
#[derive(Clone, Copy)]
pub struct RegisteredBlock {
    /// The expanded state this block should render with.
    pub expanded: bool,
    /// Whether the block cursor sits here, so the row paints the selection
    /// highlight behind itself.
    pub is_cursor: bool,
    /// Whether the row should scroll itself into view. Only ever set on the
    /// cursor block, and only for the first frame after a cursor move.
    pub scroll_into_view: bool,
}

/// The collapsible blocks of one chat, as of the last frame that rendered it,
/// plus the cursor a keybinding moves through them.
///
/// Lives on [`ChatSession`](crate::session::ChatSession) so each chat keeps its
/// own cursor.
#[derive(Default)]
pub struct BlockNav {
    /// Blocks in visual order, rebuilt every frame. Cleared and refilled so
    /// capacity is reused — no steady-state allocation.
    blocks: Vec<BlockRef>,
    cursor: Option<usize>,
    /// Set by a cursor move, consumed by the next render to scroll the cursor in.
    scroll_to_cursor: bool,
    /// That pending scroll, latched for the length of one render pass by
    /// [`begin_frame`](BlockNav::begin_frame): a row cannot know it is the
    /// cursor until it registers, so the flag has to outlive the take.
    scroll_this_frame: bool,
    /// Set by expand-all / collapse-all; the fallback for blocks with nothing
    /// stored yet, so a global expand also reaches blocks that render later
    /// (streaming in, or revealed by expanding a subagent).
    global_default: Option<bool>,
    /// A block asked for by id rather than position ([`seek`](BlockNav::seek)):
    /// the cursor lands on it when it next registers.
    seek: Option<egui::Id>,
    /// That seek, latched for one render pass like `scroll_this_frame`, so a
    /// block that never draws doesn't keep it for a later pass.
    seek_this_frame: Option<egui::Id>,
    /// A block asked for by the chat row it sits in ([`seek_row`](BlockNav::seek_row)):
    /// the cursor lands on the first block that row registers.
    seek_row: Option<usize>,
    /// That seek, latched for one render pass like `seek_this_frame`.
    seek_row_this_frame: Option<usize>,
    /// The row being drawn is the one `seek_row_this_frame` names, so the
    /// next block to register takes the cursor. Set by
    /// [`enter_row`](BlockNav::enter_row).
    row_armed: bool,
    /// The chat row being drawn, for the blocks it registers to remember.
    current_row: Option<usize>,
}

impl BlockNav {
    /// Start a fresh render pass: drop last frame's registry, keeping the cursor
    /// within whatever that frame actually held.
    pub fn begin_frame(&mut self) {
        // Clamp against last frame's count *before* dropping it: a block can
        // vanish (a subagent collapses) and strand the cursor past the end.
        self.clamp_cursor();
        self.blocks.clear();
        self.scroll_this_frame = std::mem::take(&mut self.scroll_to_cursor);
        self.seek_this_frame = self.seek.take();
        self.seek_row_this_frame = self.seek_row.take();
        self.row_armed = false;
        self.current_row = None;
    }

    /// Tell the registry the transcript is about to draw chat row `row`, so
    /// a [`seek_row`](Self::seek_row) for it lands on the first block it
    /// registers. A row that registers none leaves the seek to lapse with
    /// the pass.
    pub fn enter_row(&mut self, row: usize) {
        self.row_armed = self.seek_row_this_frame == Some(row);
        self.current_row = Some(row);
    }

    /// Tell the registry the transcript is about to draw a fold standing for
    /// chat `rows`, and return whether a [`seek_row`](Self::seek_row) names
    /// one of them. The fold's own header never takes such a seek: the fold
    /// opens instead, and the row's own block takes it as it draws, from its
    /// [`enter_row`](Self::enter_row).
    pub fn enter_fold(&mut self, rows: std::ops::Range<usize>) -> bool {
        self.row_armed = false;
        self.current_row = Some(rows.start);
        self.seek_row_this_frame
            .is_some_and(|row| rows.contains(&row))
    }

    /// Record a collapsible block at its position in the transcript and hand
    /// back everything the row needs to draw itself: its expanded state, and
    /// whether it is the cursor (and so highlights, and maybe scrolls in).
    ///
    /// A manual click writes `insert_temp` and so outranks
    /// [`global_default`](Self::global_default), which in turn outranks the
    /// site's own `default_open`.
    pub fn register(&mut self, id: egui::Id, default_open: bool, ui: &egui::Ui) -> RegisteredBlock {
        let idx = self.blocks.len();
        self.blocks.push(BlockRef {
            id,
            default_open,
            row: self.current_row,
        });
        if self.seek_this_frame == Some(id) || self.row_armed {
            self.seek_this_frame = None;
            self.seek_row_this_frame = None;
            self.row_armed = false;
            self.cursor = Some(idx);
            self.scroll_this_frame = true;
        }
        let is_cursor = self.is_cursor(idx);
        RegisteredBlock {
            expanded: self.resolve(ui.data(|d| d.get_temp(id)), default_open),
            is_cursor,
            scroll_into_view: is_cursor && self.scroll_this_frame,
        }
    }

    /// Whether `idx`, a position in this frame's registration order, is the
    /// cursor. Registration hands this back directly, so nothing outside needs
    /// to carry an index around.
    fn is_cursor(&self, idx: usize) -> bool {
        self.cursor == Some(idx)
    }

    /// Put the cursor on the block registered under `id`, and scroll to it,
    /// on the next render pass. For a caller outside the transcript, which
    /// knows a block's id but not its position. Dropped if that pass doesn't
    /// draw the block.
    pub fn seek(&mut self, id: egui::Id) {
        self.seek = Some(id);
    }

    /// Put the cursor on the first block chat row `row` registers, and
    /// scroll to it, on the next render pass: for a caller that knows where
    /// in the chat a block is but not its id, which is scoped to the
    /// transcript's `Ui`. Dropped if that pass doesn't draw such a block.
    pub fn seek_row(&mut self, row: usize) {
        self.seek_row = Some(row);
    }

    /// The cursor's position in registration order, if any.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// The chat row the cursor's block draws in, as of the last frame that
    /// drew it: a fold's first row for a fold's header. `None` with no
    /// cursor.
    pub fn cursor_row(&self) -> Option<usize> {
        self.cursor_block().and_then(|block| block.row)
    }

    /// Where the cursor is, for the jump list; `None` with no cursor.
    pub fn spot(&self) -> Option<BlockSpot> {
        let index = self.cursor?;
        let block = self.blocks.get(index)?;
        Some(BlockSpot {
            index,
            id: block.id,
        })
    }

    /// Put the cursor back at `spot`, one [`spot`](Self::spot) returned, and
    /// scroll to it: at its position now, then on its block if the next
    /// pass still draws it somewhere else. `None` drops the cursor.
    pub fn restore(&mut self, spot: Option<BlockSpot>) {
        let Some(spot) = spot else {
            self.clear_cursor();
            return;
        };
        self.set_cursor(spot.index);
        self.seek = Some(spot.id);
    }

    /// Move the cursor one block down, stopping at the last. With no cursor yet
    /// this selects the first block.
    pub fn down(&mut self) {
        let Some(last) = self.last_index() else {
            return;
        };
        self.set_cursor(match self.cursor {
            Some(cursor) => (cursor + 1).min(last),
            None => 0,
        });
    }

    /// Move the cursor one block up, stopping at the first. With no cursor yet
    /// this selects the *last* block: a transcript grows downwards, so the block
    /// nearest the input is the one a first `up` should reach.
    pub fn up(&mut self) {
        let Some(last) = self.last_index() else {
            return;
        };
        self.set_cursor(match self.cursor {
            Some(cursor) => cursor.saturating_sub(1),
            None => last,
        });
    }

    /// Move the cursor to the first block.
    pub fn first(&mut self) {
        if self.last_index().is_some() {
            self.set_cursor(0);
        }
    }

    /// Move the cursor to the last block.
    pub fn last(&mut self) {
        if let Some(last) = self.last_index() {
            self.set_cursor(last);
        }
    }

    /// Drop the cursor, so the transcript sticks to the bottom again.
    pub fn clear_cursor(&mut self) {
        self.cursor = None;
        self.scroll_to_cursor = false;
    }

    /// Flip the focused block's expanded state.
    pub fn toggle_cursor(&mut self, ctx: &egui::Context) {
        let Some(block) = self.cursor_block() else {
            return;
        };
        let expanded = self.resolve(ctx.data(|d| d.get_temp(block.id)), block.default_open);
        Self::store(ctx, block.id, !expanded);
    }

    /// Expand the focused block.
    pub fn open_cursor(&mut self, ctx: &egui::Context) {
        if let Some(block) = self.cursor_block() {
            Self::store(ctx, block.id, true);
        }
    }

    /// Collapse the focused block.
    pub fn close_cursor(&mut self, ctx: &egui::Context) {
        if let Some(block) = self.cursor_block() {
            Self::store(ctx, block.id, false);
        }
    }

    /// Expand every block, including ones that have not rendered yet.
    pub fn expand_all(&mut self, ctx: &egui::Context) {
        self.set_all(ctx, true);
    }

    /// Collapse every block, including ones that have not rendered yet.
    pub fn collapse_all(&mut self, ctx: &egui::Context) {
        self.set_all(ctx, false);
    }

    /// Resolve an expanded state from whatever is stored for the block: a stored
    /// value (a manual click, or an expand-all stamp) wins, then the global
    /// default, then the site's own default.
    fn resolve(&self, stored: Option<bool>, default_open: bool) -> bool {
        stored.unwrap_or_else(|| self.global_default.unwrap_or(default_open))
    }

    /// Stamp every *registered* block, and set the fallback so blocks that
    /// render later land the same way.
    fn set_all(&mut self, ctx: &egui::Context, open: bool) {
        ctx.data_mut(|d| {
            for block in &self.blocks {
                d.insert_temp(block.id, open);
            }
        });
        self.global_default = Some(open);
    }

    fn store(ctx: &egui::Context, id: egui::Id, open: bool) {
        ctx.data_mut(|d| d.insert_temp(id, open));
    }

    fn cursor_block(&self) -> Option<BlockRef> {
        self.cursor
            .and_then(|cursor| self.blocks.get(cursor))
            .copied()
    }

    fn last_index(&self) -> Option<usize> {
        self.blocks.len().checked_sub(1)
    }

    fn set_cursor(&mut self, idx: usize) {
        self.cursor = Some(idx);
        self.scroll_to_cursor = true;
    }

    /// Place the cursor directly, for render tests that have no keybinding to
    /// move it with.
    /// The block a [`seek`](Self::seek) is waiting to land on, if any.
    #[cfg(test)]
    pub(crate) fn seeking(&self) -> Option<egui::Id> {
        self.seek
    }

    #[cfg(test)]
    pub(crate) fn seed_cursor(&mut self, idx: usize) {
        self.set_cursor(idx);
    }

    /// Register `count` blocks as a render pass would, for tests with no
    /// egui pass to draw them.
    #[cfg(test)]
    pub(crate) fn seed_blocks(&mut self, count: usize) {
        for i in 0..count {
            self.blocks.push(BlockRef {
                id: egui::Id::unique(("seeded block", i)),
                default_open: false,
                row: None,
            });
        }
    }

    fn clamp_cursor(&mut self) {
        match self.last_index() {
            None => self.cursor = None,
            Some(last) => {
                if let Some(cursor) = &mut self.cursor {
                    *cursor = (*cursor).min(last);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed the registry as a render pass would, without an egui context.
    fn seed(nav: &mut BlockNav, count: usize) {
        for i in 0..count {
            nav.blocks.push(BlockRef {
                id: egui::Id::unique(("block", i)),
                default_open: false,
                row: None,
            });
        }
    }

    #[test]
    fn cursor_clamps_at_both_ends() {
        let mut nav = BlockNav::default();
        seed(&mut nav, 3);

        nav.down();
        assert_eq!(nav.cursor(), Some(0), "first down selects the first block");
        for _ in 0..5 {
            nav.down();
        }
        assert_eq!(nav.cursor(), Some(2), "down stops at the last block");

        for _ in 0..5 {
            nav.up();
        }
        assert_eq!(nav.cursor(), Some(0), "up stops at the first block");

        nav.last();
        assert_eq!(nav.cursor(), Some(2));
        nav.first();
        assert_eq!(nav.cursor(), Some(0));
    }

    #[test]
    fn clearing_the_cursor_drops_its_pending_scroll() {
        let mut nav = BlockNav::default();
        seed(&mut nav, 2);

        nav.down();
        nav.clear_cursor();
        assert_eq!(nav.cursor(), None);
        nav.begin_frame();
        assert!(!nav.scroll_this_frame, "no cursor left to scroll to");
    }

    #[test]
    fn first_up_with_no_cursor_selects_the_last_block() {
        let mut nav = BlockNav::default();
        seed(&mut nav, 3);

        nav.up();
        assert_eq!(nav.cursor(), Some(2));
    }

    #[test]
    fn a_cursor_move_scrolls_for_one_frame() {
        let mut nav = BlockNav::default();
        seed(&mut nav, 2);

        nav.begin_frame();
        assert!(!nav.scroll_this_frame, "no move, no scroll");

        seed(&mut nav, 2);
        nav.down();
        nav.begin_frame();
        assert!(nav.scroll_this_frame, "the move is latched for this pass");

        seed(&mut nav, 2);
        nav.begin_frame();
        assert!(!nav.scroll_this_frame, "and not for the frame after it");
    }

    #[test]
    fn begin_frame_clears_the_registry_and_clamps_the_cursor() {
        let mut nav = BlockNav::default();
        seed(&mut nav, 3);
        nav.last();
        assert_eq!(nav.cursor(), Some(2));

        // Next frame renders fewer blocks: the cursor lands on the new last.
        nav.begin_frame();
        assert!(nav.blocks.is_empty(), "the registry is rebuilt every frame");
        seed(&mut nav, 2);
        nav.begin_frame();
        assert_eq!(nav.cursor(), Some(1));

        // And a frame with no blocks at all drops the cursor entirely.
        nav.begin_frame();
        assert_eq!(nav.cursor(), None);
    }

    /// A seek lands the cursor on its block when that block registers, and
    /// scrolls to it; one whose block didn't draw that pass is dropped.
    #[test]
    fn a_seek_lands_on_its_block_for_one_pass() {
        let ctx = egui::Context::default();
        let target = egui::Id::unique("target");
        let pass = |nav: &mut BlockNav| {
            let mut registered = None;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                nav.begin_frame();
                nav.register(egui::Id::unique("a"), false, ui);
                registered = Some(nav.register(target, false, ui));
            });
            // The font atlas upload nobody renders.
            output.textures_delta.clear();
            registered.unwrap()
        };

        let mut nav = BlockNav::default();
        nav.seek(target);
        let block = pass(&mut nav);
        assert!(block.is_cursor && block.scroll_into_view);
        assert_eq!(nav.cursor(), Some(1));

        // A seek for a block that doesn't draw is gone after that pass.
        let mut nav = BlockNav::default();
        nav.seek(egui::Id::unique("elsewhere"));
        nav.begin_frame();
        nav.begin_frame();
        assert_eq!(nav.seek_this_frame, None);
        assert!(!pass(&mut nav).is_cursor);
    }

    /// A row seek lands on the first block its row registers, not on a
    /// block of a row before it or a second block of the same row.
    #[test]
    fn a_row_seek_lands_on_its_rows_first_block() {
        let ctx = egui::Context::default();
        let mut nav = BlockNav::default();
        nav.seek_row(2);
        let mut cursor_rows = Vec::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            nav.begin_frame();
            // Row 0 has a block, row 1 none, row 2 two.
            let blocks: [&[&str]; 3] = [&["r0"], &[], &["r2a", "r2b"]];
            for (row, ids) in blocks.iter().enumerate() {
                nav.enter_row(row);
                for id in *ids {
                    if nav.register(egui::Id::unique(id), false, ui).is_cursor {
                        cursor_rows.push(*id);
                    }
                }
            }
        });
        output.textures_delta.clear();
        assert_eq!(cursor_rows, ["r2a"]);
        assert_eq!(nav.cursor(), Some(1));
        // And remembers the row it sits in, for a range to start from.
        assert_eq!(nav.cursor_row(), Some(2));

        // Consumed: the next pass leaves the cursor where it landed.
        nav.begin_frame();
        assert_eq!(nav.seek_row_this_frame, None);
    }

    #[test]
    fn a_stored_value_beats_the_global_default() {
        let nav = BlockNav {
            global_default: Some(true),
            ..Default::default()
        };

        assert!(
            nav.resolve(None, false),
            "global default beats the site default"
        );
        assert!(
            !nav.resolve(Some(false), true),
            "a stored value beats the global default"
        );
        assert!(
            !BlockNav::default().resolve(None, false),
            "with no global default the site default stands"
        );
    }
}
