# Separate "this device published it" from "the poll processed it" in Dave

`seen_note_ids` holds both, so a rebuild or restore has to guess which a note
is, and bugs where a note is claimed but never processed slip in silently.

- headway:dave/found-logic-despair — a rebuild quietly claimed notes the poll hadn't processed; wanted a typed Delivered{key, id} record
- headway:dave/asthma-mix-wrap — the restore/poll delivery cursor was hard to see because seen_note_ids means both "displayed" and "processed"
- headway:dave/pledge-pilot-bind — the capped rebuild had to reconstruct which notes were own sends from unindexed_self_notes; a separate own-note set would make the cap exception a one-line read
