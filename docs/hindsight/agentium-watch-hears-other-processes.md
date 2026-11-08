# An agentium watch that hears notes another process wrote

A watch on the shared cache misses revisions another agentium run synced first.
Polling the cache every second works around it, re-reading a window each time.

- headway:dave/improve-cat-warm — done notifications went missing whenever a `log`/`send` ran alongside; took most of the debugging to find, and `watch --jsonl` now polls the cache for state revisions
