# store::apply reports the ids of the comments it signed

An action's events are signed inside apply, so a caller that wants to name them
(a word-id in a session message) can't; it only learns AddCard's id.

- headway:headway/notice-knife-please — the review message into the session can't name each comment's word-id to reply to, so it points the agent at `headway show` instead
- headway:personal/heavy-exchange-mail — `review --explainer-source` needs the record id it just signed to hang sources on, so AddReview grew a `sources` field instead of the CLI chaining AddReviewSources
- headway:notedeck/coyote-color-quantum — `nd headway` edits ack before nostrdb has unwrapped their sealed rumor, so back-to-back edits race (4 of 5 `add`→`title` pairs said "no card matching", and `block B A` right after `block A B` let a cycle through); holding the reply until the write reads back needs every id `apply` signed, and it only reports an add's
