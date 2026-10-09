# A nostrdb wait that fails fast on a dead subscription instead of hanging

`wait_for_notes` unsubscribes when it returns and polling a gone sub reads as
"no notes yet", so a second wait on the same sub hangs silently.

- headway:headway/tree-produce-ivory — the wait_for_notes Drop/unsubscribe bug cost most of the session as a silent hang
- headway:dave/exhibit-social-spawn — ndb_poll_for_notes returns 0 for both "empty" and "no such sub", so the fix could only document the trap, not fail fast
