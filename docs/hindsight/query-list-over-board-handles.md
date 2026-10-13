# `query::list` over boards that aren't one slice

`query::list` takes `&[BoardView]`, but the GUI holds each board as its own fold
handle, so it walks the boards itself with `IssueFilter::matches` instead.

- headway:headway/match-belt-quit — `ui/list.rs` `ListState::refresh` re-implements `list`'s board/column/card walk to get row indices and skip a deep copy into one slice; grouping and sorting on later cards will have to land twice
- headway:headway/album-trouble-treat — `is:archived` rows had to be added three times: in `query::list`, in `ListState::refresh` (a new `LineSlot` index form), and in the archived sheet's own `ListRow` loop
- headway:headway/office-flower-churn — `ListState::refresh` had to rebuild a `ListRow` per line from its `LineSlot` (new `LineSlot::placement`) just to hand them to `query::group`, and `group` returns row indices rather than rows so the GUI could map them back to its lines
- headway:headway/battle-stock-expire — ordering landed twice as well: `query::list` sorts its rows, but the GUI had to get a permutation (`query::sort_order` returns indices for this) and shuffle its `ListLine`s and the rebuilt `ListRow`s by it, and compute the ready marks against the rebuilt rows
- headway:headway/brave-project-parade — a saved view's board scope had to be applied twice: `query::list` callers narrow the boards they hand it (`read_scope` in the CLI), and `ListState::refresh` skips sources by `board_scope` itself
