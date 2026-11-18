# One store for an account's PNS-wrapped replaceable records

Profiles, specs and run configs each copy one subscribe/reload/poll store.
A store over a kind and a `from_note` makes a new record just parse + build.

- headway:dave/yard-earth-panic — ~10 min: `AgentSpecs` (notedeck_dave/src/agent_specs.rs) and `load_agent_spec_records_from_ndb` are copies of `BackendProfiles` and `load_backend_profiles_from_ndb`, down to the `pns_runtime` subscribe/load wiring
