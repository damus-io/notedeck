# A Dave turn has an identity

Dave knows a turn ended but not which one; with no turn seq on `ChatSession`,
work done once per turn invents its own marker from the chat's shape.

- headway:dave/loud-review-oyster — the child report dedupes on a count of assistant rows (`AgenticSessionData::reported_assistant_rows`) because no turn seq exists to record
- headway:dave/build-soul-uncover — `send --immediate` gates its interrupt on the state status (`working`/`needs_input`), because an interrupt names no turn: the host dispatches a message before applying an interrupt in the same frame, so on an idle session it would abort the message's own turn, and a turn ending between the CLI's read and the host's apply still can
- headway:dave/artist-enemy-fine — the `turn_end` marker can't name the turn it ends, so `TurnSpan` is anchored by row index (`last_row`) and the fold places it by event order; a span could otherwise point at its turn and survive a chat replace without being re-derived
