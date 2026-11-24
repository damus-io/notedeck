# One live subscription poll that every nostrdb cache shares

EditsCache and NotesCache each hand-roll the batch drain and pending-key holdback;
every new live cache copies it and can get the ordering subtly wrong.

- headway:cashflow/crunch-grow-sugar — copied EditsCache's batch-drain and pending-retain loop into NotesCache instead of calling one helper
- headway:headway/supply-try-april — hand-rolled the shared-board leg's batch drain and pending-key retry in notedeck_headway's cache.rs beside RealtimeCache's, a third copy
