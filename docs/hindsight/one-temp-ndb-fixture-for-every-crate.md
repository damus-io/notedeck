# One temporary-nostrdb test fixture every crate can use

Each crate's tests open a throwaway nostrdb through their own `test_ndb` helper;
notedeck had nine, and agentium-core, headway, columns and dave keep more.

- headway:notedeck/problem-again-strike — folded notedeck's nine `(TempDir, Ndb)` helpers into a cfg(test) `test_util::TempNdb`; the other crates cannot reach it, so their cards will each write their own
- headway:notedeck/tourist-wire-joy — chrome's keymap/user.rs tests wrote their own ndb_for (TempDir + Ndb + add_key); TempNdb is cfg(test) in notedeck, out of reach
