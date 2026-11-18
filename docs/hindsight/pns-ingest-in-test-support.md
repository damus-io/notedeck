# agentium-core tests PNS-ingest through one shared helper

`test_support` wraps an inner note in its kind-1080 envelope, ingests it and
waits for the decrypt; today engine.rs keeps that private and loaders re-write it.

- headway:dave/shock-match-sauce — ~5 min: wrote `pns_ingest` + `pns_ndb` in session_loader.rs tests beside engine.rs's private `pns_seed`, which also doesn't wait for the decrypt
- headway:dave/yard-earth-panic — ~3 min: copied `pns_ingest` + `pns_ndb` a third time into agent_spec/record.rs tests
