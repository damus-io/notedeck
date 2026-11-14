# A write tool answers once its own event is queryable

Agentium write tools answer while nostrdb is still ingesting their event,
so a script's next `nd` read can miss the write it just made.

- headway:notedeck/symptom-grow-ancient — ~2 min: `nd agentium config rm build; nd agentium config list` still listed `build` in the live check; a second later it was gone, and the tests paper over it with a subscription wait per write
