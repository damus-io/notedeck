# One temporary-nostrdb test fixture every crate can use

`TempNdb` lives twice: notedeck's cfg(test) test_util and notedeck_testing.
headway and agentium-core sit below both and still write their own helper.

- headway:notedeck/problem-again-strike — folded notedeck's nine `(TempDir, Ndb)` helpers into a cfg(test) `test_util::TempNdb`; the other crates cannot reach it, so their cards will each write their own
- headway:notedeck/tourist-wire-joy — chrome's keymap/user.rs tests wrote their own ndb_for (TempDir + Ndb + add_key); TempNdb is cfg(test) in notedeck, out of reach
- headway:notedeck/wealth-jacket-crystal — columns had two `-> (TempDir, Ndb)` helpers and one inline copy; copied TempNdb into notedeck_testing::fixtures rather than make notedeck export test code, so the struct now lives twice
- headway:notedeck/reopen-budget-accident — headway teams.rs tests' `-> (TempDir, Ndb)` became a fourth local copy of TempNdb, since headway sits below both crates that hold one
- headway:notedeck/cigar-matrix-useless — agentium-core's test_support `temp_ndb() -> (TempDir, Ndb)` became a fifth local TempNdb; it depends on neither notedeck nor notedeck_testing
- headway:dave/name-tree-bulk — the lineage round trip (spawn command → session → state → from_note) in dave's session_commands tests built its own TempDir + Ndb + subscribe/process/wait twice per case; no shared fixture reaches notedeck_dave's lib tests
