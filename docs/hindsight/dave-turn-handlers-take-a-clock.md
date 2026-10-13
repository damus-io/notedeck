# Dave's turn handlers take a clock

`apply_response` and `handle_stream_end` read `session::now_unix()` themselves,
so a test of anything timed per turn moves a field back instead of pinning now.

- headway:dave/artist-enemy-fine — the turn-timing Fixture tests set `turn_started_at` to `now - 90` and assert a 1s window (`90..=91`) rather than an exact span, since the stream end stamps its own end
