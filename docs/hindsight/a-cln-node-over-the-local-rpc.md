# a cln Node over the local RPC

Commando is the only way to a node: a real-data check needs a rune for every call,
and on-node file imports can't date by block. A `lightning-cli` `Node` fixes both.

- headway:cashflow/choose-ice-rapid — wrote and deleted a ~60-line `Node` over `lightning-cli` to run the block-dating real-data check, since no rune permits corerpc yet; on-node imports from files still date by bookkeeper's timestamp
- headway:cashflow/gauge-unaware-day — skipped the real-data check of the once-only whole read rather than rewrite the same throwaway `lightning-cli` shim a second time; the trigger is covered only by a FakeNode test
- headway:cashflow/gospel-lunch-attract — the bcli comparison of corerpc's amounts against bitcoind's text didn't run, since the test rune denies corerpc listwallets/listtransactions; exactness rests on the double round-trip test alone
