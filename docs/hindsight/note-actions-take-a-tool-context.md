# Columns' note actions run from a tool context

Card: headway:notedeck/wait-marine-boss
Columns' post, reaction and follow senders take a `PublishApi` and the account's
`ContactState`, which a `ToolContext` lacks, so a tool re-writes each one.

- headway:dave/virus-cram-differ — `nostr_tools/act.rs` re-wrote sign → ingest → publish (`send`) and the kind-3 rebuild (`contact_list_note`) beside `publish_note_builder` and `send_kind_3_event`, and pulled `reaction_note` out of `send_reaction_event`; one sender over `(ndb, signer, relays, publisher)` would have let the tools and the buttons share every line
