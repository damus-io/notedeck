# A conversation note's role and body shape, read without rendering it

Only `render_conversation_note` says what a kind-1988 note becomes, so a
reader that skips rendering keeps a hand copy that a test holds to it.

- headway:notedeck/flower-foam-attitude — `note_body` in cli/grep.rs re-states the role → `message_role` map (and which bodies are the raw content) for the prefilter, guarded by `note_body_mirrors_the_loader`; the card's premise that raw content is a superset was wrong for `permission_response` and live `tool_call` notes, which took a ~10 min census to find
