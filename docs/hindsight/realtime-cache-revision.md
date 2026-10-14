# A revision counter on RealtimeCache

RealtimeCache refolds without a counter, so text derived off its views (a card
title for a list row) has no key to cache on and is re-resolved every frame.

- headway:dave/feature-marine-curious — the card asked callers to cache the new `KindRenderer::title` keyed on "the board cache's revision", and there is none; left to the list-row card to work out
- headway:dave/tone-grief-hour — the session list re-looks each card title up every 2s (`card_titles::REFRESH_SECS`) instead of on change, so a renamed card lags up to 2s and an unchanged one is refolded for nothing
- headway:dave/throw-draw-draw — successor chains need a card's parent and its epic's done/total, which change on the board with no signal; CardTitles now also keeps each card's Outline and regroups the list whenever a 2s re-lookup sees it differ
- headway:headway/match-belt-quit — the issue list keys its cached rows on every board's fold handle instead, through a new `BoardRef::same_fold` pointer compare and a `Vec<BoardRef>` copy of last frame's sources (`ui/list.rs` `ListInputs`)
- headway:notedeck/field-fever-follow — Cmd-K search finds sessions by their cards' titles off CardTitles, refreshed at most every 30s (`SEARCH_REFRESH_SECS`) because a refresh re-resolves every session's ref; with a revision it would refresh on change, and a card renamed shows up to 30s late
