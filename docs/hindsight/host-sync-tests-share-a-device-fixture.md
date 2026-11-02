# Host private-sync tests share a device and pump-until fixture

Each private_sync.rs e2e test re-spells a relay, a TempNdb per device and
a hand-rolled 500-step host.update loop: seven copies, ~40 lines apiece.

- headway:headway/mango-repair-obtain — the AUTH test copied the relay, three devices and three pump-until loops; a `Device { ndb, host, account }` with `pump_until(&mut [Device], pred)` would have been ~15 lines
