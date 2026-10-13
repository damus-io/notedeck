# store::apply reports the ids of the comments it signed

An action's events are signed inside apply, so a caller that wants to name them
(a word-id in a session message) can't; it only learns AddCard's id.

- headway:headway/notice-knife-please — the review message into the session can't name each comment's word-id to reply to, so it points the agent at `headway show` instead
