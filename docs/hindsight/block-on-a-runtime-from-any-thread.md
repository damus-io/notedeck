# One block-on-its-own-runtime helper, safe from any thread

A job may await on a thread already in a tokio runtime, where block_on panics;
each crate hand-rolls a scoped thread with its own runtime instead of one helper.

- headway:notedeck/resist-explain-pig — `RelayFetch::wait` (crates/notedeck/src/tool_remote.rs) is a third copy of Dave's `data_tools/control.rs` `block_on` (and a cousin of `tool_round::block_on_own_runtime` and cashflow's inline ones); notedeck had none to share, so Dave's couldn't be reused
