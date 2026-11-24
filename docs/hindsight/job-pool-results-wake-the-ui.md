# A JobPool result wakes the UI when it lands

A `JobPool` job hands back a bare oneshot, so its poller repaints every 50ms
until it lands, and a kittest sleep-spins frames to see it.

- headway:ux-feedback/rare-flash-issue — `land_saves` (`crates/notedeck_dave/src/session/screenshot.rs`) and the composer re-poll on `request_repaint_after(50ms)` like headway's `Attachments::poll` (`ui/attach.rs`), and the paste test (`paste_image` in `ui/mod.rs`) loops `thread::sleep` + `run_ok` against a deadline; a job that repaints the ctx on completion would have needed neither
