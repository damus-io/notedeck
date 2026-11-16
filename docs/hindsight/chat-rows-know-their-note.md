# A chat row knows the note it shows

Chat rows record no note id, so finding a note's row re-folds the whole session.
That is O(session) on the UI thread, and wrong for a chat not yet at its fold.

- headway:notedeck/omit-assist-miss — `session_loader::message_row` re-folds the session to land a transcript hit on its row; a per-row note id would have been a lookup
