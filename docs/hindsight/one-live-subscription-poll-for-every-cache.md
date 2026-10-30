# One live subscription poll that every nostrdb cache shares

EditsCache and NotesCache each hand-roll the batch drain and pending-key holdback;
every new live cache copies it and can get the ordering subtly wrong.

- headway:cashflow/crunch-grow-sugar — copied EditsCache's batch-drain and pending-retain loop into NotesCache instead of calling one helper
