# An HTTP client installs its own crypto provider

Card: headway:notedeck/sound-base-opinion
`HyperHttpClient::new` panics in any test that didn't call `enostr::install_crypto`
first; on a job thread the panic surfaces only as "the tool panicked".

- headway:dave/skill-club-smart — ~4 min: three web/sub-agent tests failed on rustls' "Could not automatically determine the process-level CryptoProvider", one only as a caught job panic; each test now calls `install_crypto` by hand, as `relay_limits.rs` keeps its own copy for
- headway:dave/hurry-pony-symbol — ~2 min: a new `network.rs` test panicked on the CryptoProvider; moved `relay_limits.rs`' private copy out before finding `enostr::install_crypto`, then reverted the move
