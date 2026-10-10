# The signatures gate prints the tuple it found

`no_signature_holds_a_tuple` prints a hit as `fn name -> ..`, so every hit has to
be opened in its file to see which pair it is, before any can be grouped by type.

- headway:notedeck/crew-elevator-snow — ~3 min: 52 hits in enostr printed as `-> ..`; wrote an awk loop over the hit list to print each signature before the same `(Nip11ApplyOutcome, Output)` and `(T, Vec<QueuedRelayFrame>)` pairs could be seen recurring
- headway:notedeck/census-goddess-yard — ~2 min: 27 notedeck_ui hits printed as `-> ..`; opened each in sed to see which were test-harness `(GitPatch, GitPatchState)` state (9 sites, one struct) versus one-off pairs
- headway:notedeck/fashion-color-fabric — ~2 min: 18 notedeck_headway hits printed as `-> ..`; opened each to find `(usize, usize)` was a MoveCard target in two fns and `(usize, &CardView)` a find_card with 30 callers
- headway:notedeck/oxygen-awake-lady — ~3 min: 57 notedeck_dave hits printed as `-> ..`; grepped each to find `(SessionId, String)` recurring across 8 fns in 5 files (one SessionMessage) and `(Modifiers, Key)` behind 69 test presses
