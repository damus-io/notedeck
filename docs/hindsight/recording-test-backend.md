# A recording fake AiBackend in test_support

Each Dave test that needs a backend writes its own ~40-line no-op `AiBackend` impl (`ScriptBackend` in convergence_tests.rs, `CompactProbe` in stream_events.rs) just to see which turns and commands reached it.

- headway:dave/worry-walnut-zoo — ~5 min: wrote `CompactProbe` to see that COMPACT sent a `/compact` turn and no direct compact; a `test_support::RecordingBackend` logging each call (turns, compacts, interrupts, mode changes) would have left only the asserts
