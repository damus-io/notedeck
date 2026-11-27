# A nostrdb wait for n notes that waits for n

`wait_for_notes(sub, n)` returns at the writer's first batch, so a test races
its own later notes; ~40 call sites use it over `wait_for_all_notes_within`.

- headway:notedeck/basket-say-force — columns bar_search's `ingest` returned with only the kind-1 note, so the "quokka" profile search found nothing on Linux CI; read nostrdb-rs's source to learn n is a cap, not a count
