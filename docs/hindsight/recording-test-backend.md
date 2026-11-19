# A recording fake AiBackend in test_support

Card: headway:notedeck/simple-race-leg
Each Dave test needing a backend writes its own ~40-line no-op AiBackend impl
(ScriptBackend, CompactProbe) just to see which turns and commands reached it.

- headway:dave/worry-walnut-zoo — ~5 min: wrote `CompactProbe` in stream_events.rs to see that COMPACT sent a `/compact` turn and no direct compact; a `test_support::RecordingBackend` logging each call (turns, compacts, interrupts, mode changes) would have left only the asserts
- headway:dave/success-raw-math — ~5 min: wrote `HandedStream` in stream_events.rs, a no-op AiBackend whose only job is to hand the test the turn's response sender, to drive tool rounds through `process_events`; a test_support backend that records calls and hands back each turn's sender would have done both jobs
- headway:dave/evoke-blanket-little — ~3 min: grew `HandedStream` an `offered` list to see which tools a spec session's request advertised; a recording test_support backend would have logged the tool names with the turn
- headway:dave/picnic-diagram-dizzy — ~4 min: wrote `Probe` in spec_runner.rs, a fifth no-op AiBackend, to see that a spec session's turn reached its runner (with which tools) and that its stop went to the same one; a recording test_support backend logging turns and interrupts would have left only the asserts
