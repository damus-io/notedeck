# Memory Stats

[![Crates.io](https://img.shields.io/crates/v/memory-stats.svg)](https://crates.io/crates/memory-stats)
[![License](https://img.shields.io/crates/l/memory-stats)](https://github.com/Arc-blroth/memory-stats)
[![Build Status](https://github.com/Arc-blroth/memory-stats/workflows/CI/badge.svg)](https://github.com/Arc-blroth/memory-stats/actions?query=workflow:"CI")
![Dragon Powered](https://img.shields.io/badge/%F0%9F%90%89-dragon%20powered-brightgreen)

A cross-platform memory profiler for Rust, supporting Windows, Linux, and MacOS. This crate provides two metrics:

- **"Physical" Memory**, which corresponds to the _Resident Set Size_ on Linux and MacOS and the _Working Set_ on Windows.
- **"Virtual" Memory**, which corresponds to the _Virtual Size_ on Linux and MacOS and the _Pagefile Usage_ on Windows.

## Usage

Add `memory-stats` as a dependency to your `Cargo.toml`:

```toml
[dependencies]
memory-stats = "1.2.0"
```

### Optional Features

`serde`: Enables serialization and deserialization of the `MemoryStats` struct.

## Example

Here's an example that prints out the current memory usage:

```rs
use memory_stats::memory_stats;

fn main() {
    if let Some(usage) = memory_stats() {
        println!("Current physical memory usage: {}", usage.physical_mem);
        println!("Current virtual memory usage: {}", usage.virtual_mem);
    } else {
        println!("Couldn't get the current memory usage :(");
    }
}
```

## Caveats

Getting accurate memory usage on Linux is fairly expensive and not always possible. This crate always attempts to use the statistics from
[`/proc/self/smaps`](https://man7.org/linux/man-pages/man5/proc.5.html#:~:text=See%20user_namespaces%287%29.-,/proc/%5Bpid%5D/smaps,-%28since%20Linux%202.6.14)
if avaliable. However, since support for `/proc/self/smaps` might not be compiled in on all kernels, this crate will also use the faster but less accurate statistics from
[`/proc/self/statm`](https://man7.org/linux/man-pages/man5/proc.5.html#:~:text=by%0A%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20waitpid%282%29.-,/proc/%5Bpid%5D/statm,-Provides%20information%20about)
as a fallback.

If speed is needed over accuracy, the `always_use_statm` feature can be enabled to always use the `/proc/self/statm` statistics.

## License

This crate is dual-licensed under either:

- the [Apache License, Version 2.0](LICENSE-APACHE)
- the [MIT license](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
