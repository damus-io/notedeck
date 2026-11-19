# A read tool can fetch what nostrdb lacks

Card: headway:notedeck/resist-explain-pig
Left: a fetch waits for notes or its bound, not EOSE, so an unanswered miss costs
the whole bound; the act tools can't wait, so they fetch and ask for a retry.

- headway:dave/pear-spell-world — the nostr read tools answer `found: false` / `missing` instead of fetching; a `ToolContext` one-shot relay query (a `SlowTool` that subscribes, waits for EOSE with a bound, then reads ndb) would make `nd nostr profile|thread|timeline` work on anything, not only what the columns already loaded
- headway:dave/virus-cram-differ — `nostr_reply`, `nostr_react` and `nostr_zap` refuse a note nostrdb lacks (a reply needs its thread tags, a zap its author), so an agent can only act on what the columns already loaded; the error tells it to open the thread and retry
