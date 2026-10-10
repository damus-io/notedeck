# The signatures gate prints the tuple it found

`no_signature_holds_a_tuple` prints a hit as `fn name -> ..`, so every hit has to
be opened in its file to see which pair it is, before any can be grouped by type.

- headway:notedeck/crew-elevator-snow — ~3 min: 52 hits in enostr printed as `-> ..`; wrote an awk loop over the hit list to print each signature before the same `(Nip11ApplyOutcome, Output)` and `(T, Vec<QueuedRelayFrame>)` pairs could be seen recurring
