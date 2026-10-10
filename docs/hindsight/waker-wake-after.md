# A delayed wake on notedeck::Waker

`Waker` only wakes now and `update` has no egui context, so per-frame work that
holds something back spawns its own sleeping thread to come back to it.

- headway:dave/spirit-tower-opera — the inspector snapshot's debounce needed a trailing wake; wrote Dave::schedule_snapshot_wake (a std::thread sleep + wake, deduped by snapshot_wake_at) for want of Waker::wake_after
