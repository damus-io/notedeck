# a fake commando peer

A test can stand a `Node` in for the node, but nothing stands in for the lightning peer on the far side of lnsocket, so `Commando` itself (the handshake, the rune, a pipelined batch over one connection) is never run by a test.

- headway:cashflow/control-lemon-satoshi — `Commando::call_each` pipelines a batch over one `CommandoClient` and no test reaches it; only the sequential default is covered, and the batch's order and per-call errors rest on reading lnsocket's demux
- headway:cashflow/blur-pudding-pigeon — FakeNode denied with a made-up `1502 Not authorized`; the real code (19537, `Invalid rune: Not permitted: …`) came from reading lnsocket's own test, and the pinned rune's restrictions are never run through a real rune check
- headway:cashflow/differ-guess-bright — the one-connection `Commando` (lazy connect, reconnect after a lost call, hang-up on drop) has no test; it was checked only by counting `connect()`s to 9735 under strace in a live `import cln` (3 before, 1 after)
