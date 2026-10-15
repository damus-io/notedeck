# The cold-open test waits for the ingest instead of one frame

open_tests' cold-open-lands-in-dave case retries one frame after seeding, so it
fails ~1 in 3 parallel chrome lib runs on master too; you rerun to clear it.

- headway:notedeck/hill-measure-accuse — 6 min: a red chrome lib run sent me back to the base commit to prove the flake wasn't mine
- headway:notedeck/deny-abuse-able — 4 min: the new chrome_open agentium test copied the one-frame retry and flaked the same way; it now polls until resolved, but the cold-open test itself still doesn't
