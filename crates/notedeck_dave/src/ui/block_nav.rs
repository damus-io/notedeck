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
        self.blocks.push(BlockRef { id, default_open });
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

    /// The cursor's position in registration order, if any.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
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
    #[cfg(test)]
    pub(crate) fn seed_cursor(&mut self, idx: usize) {
        self.set_cursor(idx);
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
                id: egui::Id::new(("block", i)),
                default_open: false,
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
