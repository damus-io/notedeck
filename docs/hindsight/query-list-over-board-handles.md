# `query::list` over boards that aren't one slice

`query::list` takes `&[BoardView]`, but the GUI holds each board as its own fold
handle, so it walks the boards itself with `IssueFilter::matches` instead.

- headway:headway/match-belt-quit — `ui/list.rs` `ListState::refresh` re-implements `list`'s board/column/card walk to get row indices and skip a deep copy into one slice; grouping and sorting on later cards will have to land twice
- headway:headway/album-trouble-treat — `is:archived` rows had to be added three times: in `query::list`, in `ListState::refresh` (a new `LineSlot` index form), and in the archived sheet's own `ListRow` loop
