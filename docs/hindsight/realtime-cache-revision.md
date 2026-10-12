# A revision counter on RealtimeCache

RealtimeCache refolds without a counter, so text derived off its views (a card
title for a list row) has no key to cache on and is re-resolved every frame.

- headway:dave/feature-marine-curious — the card asked callers to cache the new `KindRenderer::title` keyed on "the board cache's revision", and there is none; left to the list-row card to work out
