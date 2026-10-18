# The agentium: ref from the environment, in one shared helper

Card: headway:headway/peasant-sock-steel
Every CLI that stamps its writes with the session re-reads and re-validates
`$AGENTIUM_SESSION` on its own (headway_cli's review.rs, now cashflow's rules).

- headway:cashflow/wrestle-depart-cash — wrote `rule_cmd::session_ref` on `wordid::WordIdRef` because peasant-sock-steel's shared helper hasn't landed
- headway:cashflow/name-draw-animal — moved `session_ref` to `vault_cmd` so the edit commands stamp their notes with it too: a second caller in one CLI, still not shared across CLIs
