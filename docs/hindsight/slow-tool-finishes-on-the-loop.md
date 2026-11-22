# A slow tool can finish its write back on the render loop

A `SlowTool`'s job runs off the loop and never gets a `ToolContext` back, so
a write needing a network round trip first is two tools the caller chains.

- headway:ux-feedback/guess-kiwi-cause — `comment --attach` couldn't upload then post in one tool: the upload is `headway_attach` (`crates/notedeck_headway/src/tools/attach.rs`), the comment takes its answer as `attachments`, and nd chains them through a new `x-nd-via` spec key (`ToolArg::via`, `nd_cli::schema::take_via`); a `SlowTool::finish(cx, work)` step the socket runs on the loop after the job would have made it one tool that a model calls once too
