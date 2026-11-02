# private fan-out routes by kind

Private publish helpers take a bare relay list, so each caller picks the set, and
one that picks wrong sends plaintext to an encrypted-only relay.

- headway:headway/tumble-wet-topple — read through six fan-out call sites (headway app and tools, horizon, dave, the host, the 10013 publish) to sort each into plaintext or envelopes; only headway's took `PrivateRelayTargets`, the rest still pass a list they chose
