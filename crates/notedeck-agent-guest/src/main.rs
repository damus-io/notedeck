//! `notedeck-agent`: the tool executor a sandbox guest runs.
//!
//! ```text
//! notedeck-agent [--root <dir>] [--ipc-socket <path> --ipc-vsock-port <port>]
//!                                 serve tool calls on stdio, paths relative to <dir>
//!                                 (default: the current directory)
//! notedeck-agent --version        print the build and protocol, and exit
//! ```
//!
//! The protocol is `notedeck_agent_proto::wire`. The binary holds no
//! credential and its tool calls come only over stdio: the model
//! conversation, the permission gate and the diff UI all stay on the host.
//!
//! With the two `--ipc-*` flags it also serves notedeck's own socket at
//! `<path>`, spliced onto vsock `<port>` on the host
//! (`notedeck_agent_guest::ipc_bridge`), so `nd` works inside the guest.
//! The host passes them only for a sandbox it lets reach notedeck.

use std::path::PathBuf;
use std::process::ExitCode;

use notedeck_agent_guest::{serve, Executor};
use notedeck_agent_proto::ipc;
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

const USAGE: &str = "usage: notedeck-agent [--root <dir>] \
                     [--ipc-socket <path> --ipc-vsock-port <port>] | --version";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut root = None;
    let mut ipc_socket = None;
    let mut ipc_port = None;
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
            Some(ipc::SOCKET_FLAG) => {
                let Some(path) = args.next() else {
                    eprintln!("{} needs a path\n{USAGE}", ipc::SOCKET_FLAG);
                    return ExitCode::from(2);
                };
                ipc_socket = Some(PathBuf::from(path));
            }
            Some(ipc::VSOCK_PORT_FLAG) => {
                let Some(port) = args.next().and_then(|p| p.to_str()?.parse::<u32>().ok()) else {
                    eprintln!("{} needs a port number\n{USAGE}", ipc::VSOCK_PORT_FLAG);
                    return ExitCode::from(2);
                };
                ipc_port = Some(port);
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

    match (ipc_socket, ipc_port) {
        (None, None) => {}
        (Some(socket), Some(port)) => start_ipc_bridge(&socket, port),
        _ => {
            eprintln!(
                "{} and {} go together\n{USAGE}",
                ipc::SOCKET_FLAG,
                ipc::VSOCK_PORT_FLAG
            );
            return ExitCode::from(2);
        }
    }

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

/// Serve notedeck's socket at `socket` in the guest, spliced onto vsock
/// `port` on the host. A bridge that won't start is reported and skipped:
/// the tool calls don't need it, and `nd` then fails on its own with no
/// socket to reach.
#[cfg(target_os = "linux")]
fn start_ipc_bridge(socket: &std::path::Path, port: u32) {
    use notedeck_agent_guest::ipc_bridge;
    let connect = move || ipc_bridge::vsock_connect(libc::VMADDR_CID_HOST, port);
    if let Err(e) = ipc_bridge::spawn(socket, connect) {
        eprintln!("notedeck-agent: no ipc bridge at {}: {e}", socket.display());
    }
}

/// vsock is a Linux guest's; the binary only ever runs in one.
#[cfg(not(target_os = "linux"))]
fn start_ipc_bridge(socket: &std::path::Path, _port: u32) {
    eprintln!(
        "notedeck-agent: no ipc bridge at {}: vsock needs a Linux guest",
        socket.display()
    );
}
