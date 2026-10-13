# Time a fold against a clone of a real db

Nothing times a cache's seed fold against a real nostrdb, so measuring a
startup fold means writing, then deleting, a scratch example over an APFS clone.

- headway:dave/cluster-rare-absurd — timed `fold_sessions` on a clone of jb55's 14 GB db (276 ms cold, 73 ms warm) with a scratch `agentium-core` example, deleted before the commit
- headway:dave/abuse-doll-joy — measured bytes per kind, per session and per LMDB database on a clone of jb55's 14 GB db with a scratch crate pinned to the unpushed `Ndb::stat` rev, plus `mdb_stat` rebuilt from nostrdb's own lmdb (homebrew's rejects the file); none of it survives the session
