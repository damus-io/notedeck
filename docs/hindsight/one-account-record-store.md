# One store for an account's PNS-wrapped replaceable records

Card: headway:notedeck/wood-cube-any
Profiles and specs share `AccountRecords<T>` now; run configs are left. They
fold one note at a time with no id tie-break: their own card.

- headway:dave/yard-earth-panic — ~10 min: `AgentSpecs` (notedeck_dave/src/agent_specs.rs) and `load_agent_spec_records_from_ndb` are copies of `BackendProfiles` and `load_backend_profiles_from_ndb`, down to the `pns_runtime` subscribe/load wiring
