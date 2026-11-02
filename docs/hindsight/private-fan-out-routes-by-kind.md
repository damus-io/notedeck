# private fan-out routes by kind

The private publish helpers take a bare relay list, so each caller picks the plaintext or the envelope set by hand.
A call site that picks wrong sends plaintext to an encrypted-only relay; one target that routes every frame by kind would make that impossible.

- headway:headway/tumble-wet-topple — read through six fan-out call sites (headway app and tools, horizon, dave, the host, the 10013 publish) to sort each into plaintext or envelopes; only headway's took `PrivateRelayTargets`, the rest still pass a list they chose
