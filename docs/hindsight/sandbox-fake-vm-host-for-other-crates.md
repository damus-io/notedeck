# A fake VmHost other crates' tests can use

agentium-sandbox's FakeVms is cfg(test), so a crate driving fleet::Guests
writes its own fake VmHost, and can't easily copy its real-guest connect.

- headway:dave/gesture-cage-west — wrote a records-only fake `VmHost` in `notedeck_dave/src/sandbox.rs` tests to check the startup sweep
