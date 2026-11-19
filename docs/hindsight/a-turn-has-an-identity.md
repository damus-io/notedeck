# A Dave turn has an identity

Card: headway:notedeck/lucky-music-horror
Local turns have a `TurnSeq`. Left: (a) turn_end marker tag + `TurnSpan.seq`,
(b) OpenAiBackend keys runs/stops by it, (c) interrupt names a seq (control.rs).

- headway:dave/loud-review-oyster — the child report dedupes on a count of assistant rows (`AgenticSessionData::reported_assistant_rows`) because no turn seq exists to record
- headway:dave/build-soul-uncover — `send --immediate` gates its interrupt on the state status (`working`/`needs_input`), because an interrupt names no turn: the host dispatches a message before applying an interrupt in the same frame, so on an idle session it would abort the message's own turn, and a turn ending between the CLI's read and the host's apply still can
- headway:dave/artist-enemy-fine — the `turn_end` marker can't name the turn it ends, so `TurnSpan` is anchored by row index (`last_row`) and the fold places it by event order; a span could otherwise point at its turn and survive a chat replace without being re-derived
- headway:dave/certain-luggage-design — the native loop's run (a user message and every tool round it sets off) has no id, so `OpenAiBackend` reads it off the history's shape (`starts_run`: the last message is the user's; `tool_steps`: tool-call rows since it) and keys its stop flag by session, so a stop can't name the run it stops
- headway:notedeck/lucky-music-horror — the child report finds the reported turn's row by counting spans back from the last (`reported_turn_end`), because `TurnSpan` has no seq to match; (a) would make it a lookup
