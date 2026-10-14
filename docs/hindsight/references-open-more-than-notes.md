# A resolved reference can name a profile or a hashtag, not only a note

`ResolvedRef` holds a `NoteId` alone, so `AppAction::Open` of an `npub` or a
`#tag` resolves to nothing; each such target needs its own check ahead of resolve.

- headway:notedeck/jump-bike-wife — Columns' profile and hashtag search hits had no uri that opens; added `notedeck::reference_note_action` as a side path the chrome's Open branch tries before `resolve_reference`, instead of the parser registry knowing them
