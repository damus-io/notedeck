# A read tool can fetch what nostrdb lacks

A `ToolContext` has no relay pool or one-shot fetch, so a nostr read tool
answers only what this notedeck already pulled: an unseen note dead-ends.

- headway:dave/pear-spell-world — the nostr read tools answer `found: false` / `missing` instead of fetching; a `ToolContext` one-shot relay query (a `SlowTool` that subscribes, waits for EOSE with a bound, then reads ndb) would make `nd nostr profile|thread|timeline` work on anything, not only what the columns already loaded
