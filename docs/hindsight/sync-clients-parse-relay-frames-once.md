# sync clients parse relay frames once

nostrdb_net's RelayMessage lacks AUTH, CLOSED and NEG-*, so Session and sync::Relay
prefix-match frames and re-parse the JSON in each handler; one typed parse would do.

- headway:headway/able-couple-scare — wrote five ad hoc `Vec<Value>` parsers for AUTH, OK and CLOSED across session.rs and relay.rs, and lost a test run to `next_text` swallowing the AUTH `OK` that `await_auth` was waiting for
