# a cln Node over the local RPC

The `cln` importer reaches a node only over commando, so a check against real data needs a rune that permits every call, and an import run on the node itself goes through saved listings, which can't ask corerpc for block times. A committed read-only `Node` over `lightning-cli` (or the RPC socket) would make both a flag rather than a throwaway shim.

- headway:cashflow/choose-ice-rapid — wrote and deleted a ~60-line `Node` over `lightning-cli` to run the block-dating real-data check, since no rune permits corerpc yet; on-node imports from files still date by bookkeeper's timestamp
