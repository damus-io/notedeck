# a fake commando peer

A test can stand a `Node` in for the node, but nothing stands in for the lightning peer on the far side of lnsocket, so `Commando` itself (the handshake, the rune, a pipelined batch over one connection) is never run by a test.

- headway:cashflow/control-lemon-satoshi — `Commando::call_each` pipelines a batch over one `CommandoClient` and no test reaches it; only the sequential default is covered, and the batch's order and per-call errors rest on reading lnsocket's demux
