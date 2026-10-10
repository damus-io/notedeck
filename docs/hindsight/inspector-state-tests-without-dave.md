# Inspector state tests that build without notedeck_dave

The inspector's state modules (transcript scans, git and temp-dir listers) use
no egui, but live in notedeck_dave, so each unit test relinks its test binary.

- headway:dave/shrimp-guide-poet — 120-min session: at load 30–68 each `cargo test -p notedeck_dave --lib` cycle took 5–30 min to rebuild (five cycles, one killed at the 30-min cap) for 6 pure tests of inspector/outputs.rs (mentions scan, git log lister); in agentium-core, beside Message, they'd have built in a fraction of that
