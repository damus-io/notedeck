# Dave's per-frame polls take what they read, not the whole AppContext

Dave's polls take `&mut AppContext` to read a store and a flag; TestDave has none,
so a poll's first test narrows its signature, and the unnarrowed stay untested.

- headway:dave/rough-crash-decline — the kind-31988 discovery poll (`poll_session_state_events`) had no test because it took `&mut AppContext` for `ndb` and `private_sync_settled`; narrowed it to those two before its containment test could be written
