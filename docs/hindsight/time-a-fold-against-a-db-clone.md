# Time a fold against a clone of a real db

Nothing times a cache's seed fold against a real nostrdb, so measuring a
startup fold means writing, then deleting, a scratch example over an APFS clone.

- headway:dave/cluster-rare-absurd — timed `fold_sessions` on a clone of jb55's 14 GB db (276 ms cold, 73 ms warm) with a scratch `agentium-core` example, deleted before the commit
- headway:dave/abuse-doll-joy — measured bytes per kind, per session and per LMDB database on a clone of jb55's 14 GB db with a scratch crate pinned to the unpushed `Ndb::stat` rev, plus `mdb_stat` rebuilt from nostrdb's own lmdb (homebrew's rejects the file); none of it survives the session
- headway:notedeck/cement-grass-nice — timed `grep::search` (5.5 s warm, 14–78 s cold) and three alternatives on a copy of jb55's 12 GB db with a scratch `agentium-core` example plus a scratch crate over a patched nostrdb checkout; both kept only under /titan, nothing reusable landed
- headway:notedeck/circle-merge-post — re-timed the landed transcript `scan` on the spike's db copy (0.48–0.54 s warm, 51 s cold) through a scratch `#[ignore]` test in transcript_search.rs, deleted before the commit
- headway:notedeck/flower-foam-attitude — ~15 min: two full runs of a scratch parity bench (44 pattern × view runs at ~6 s each on the old path); timed the prefilter against the full load on the spike's db copy (6 s → 0.7–1.6 s for rare patterns, exact parity over 44 pattern × view runs) with a scratch `agentium-core` example, deleted before the commit
