//! `notedeck-agent`: the tool executor a sandbox guest runs.
//!
//! ```text
//! notedeck-agent [--root <dir>]   serve tool calls on stdio, paths relative to <dir>
//!                                 (default: the current directory)
//! notedeck-agent --version        print the build and protocol, and exit
//! ```
//!
//! The protocol is `notedeck_agent_proto::wire`. The binary holds no
//! credential and talks to nothing but its stdio: the model conversation,
//! the permission gate and the diff UI all stay on the host.

use std::path::PathBuf;
use std::process::ExitCode;

use notedeck_agent_guest::{serve, Executor};
use notedeck_agent_proto::wire::{Version, PROTOCOL_VERSION};

/// The commit this binary was built from, stamped by
/// `scripts/build-guest-agent`; the host refuses a guest whose build isn't
/// its own. A plain `cargo build` leaves it unset.
const BUILD: &str = match option_env!("NOTEDECK_AGENT_BUILD") {
    Some(build) => build,
    None => "unstamped",
};

/// Worker threads for the calls in flight. Grep and Glob walk the tree
/// synchronously, so each occupies a worker until it's done; with a few,
/// one slow search can't hold up the others or a cancel. A guest may have
/// a single vCPU, so this isn't the CPU count.
const WORKERS: usize = 4;

const USAGE: &str = "usage: notedeck-agent [--root <dir>] | --version";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--version") => {
                println!("notedeck-agent {BUILD} (protocol {PROTOCOL_VERSION})");
                return ExitCode::SUCCESS;
            }
            Some("--root") => {
                let Some(dir) = args.next() else {
                    eprintln!("--root needs a directory\n{USAGE}");
                    return ExitCode::from(2);
                };
                root = Some(PathBuf::from(dir));
            }
            _ => {
                eprintln!("unknown argument {arg:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let root = match root.map_or_else(std::env::current_dir, Ok) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("notedeck-agent: no working directory: {e}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .enable_all()
        .build()
        .expect("a tokio runtime starts");
    let served = runtime.block_on(serve(
        Executor::new(root),
        Version::new(BUILD),
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    ));
    // tokio reads stdin on a blocking thread, which a Shutdown leaves parked
    // in read(2); waiting for it on drop would hang until the host wrote again.
    runtime.shutdown_background();
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("notedeck-agent: {e}");
            ExitCode::FAILURE
        }
    }
}
