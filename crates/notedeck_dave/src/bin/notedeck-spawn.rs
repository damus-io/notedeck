//! CLI tool to spawn a new agent in a running notedeck instance.
//!
//! Usage:
//!   notedeck-spawn              # spawn with current directory
//!   notedeck-spawn /path/to/dir # spawn with specific directory

#[cfg(unix)]
fn main() {
    use std::path::PathBuf;

    // Parse arguments
    let cwd = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Canonicalize path
    let cwd = match cwd.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Invalid path '{}': {}", cwd.display(), e);
            std::process::exit(1);
        }
    };

    // Validate it's a directory
    if !cwd.is_dir() {
        eprintln!("Error: '{}' is not a directory", cwd.display());
        std::process::exit(1);
    }

    use notedeck_ipc::{Client, Outcome, Reply, Request};

    let socket_path = notedeck_ipc::socket_path();

    // Connect to the running notedeck instance
    let mut client = match Client::connect(&socket_path) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Could not connect to notedeck at {}", socket_path.display());
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("Is notedeck running? Start it first with `notedeck`");
            std::process::exit(1);
        }
    };

    let response = match client.request(&Request::SpawnAgent { cwd }) {
        Ok(response) => response,
        Err(e) => {
            eprintln!("Failed to spawn: {}", e);
            std::process::exit(1);
        }
    };

    match response.outcome {
        Outcome::Ok(Reply::Spawned { session_id }) => {
            println!("Agent spawned (session {})", session_id);
        }
        Outcome::Ok(other) => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Outcome::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("notedeck-spawn is only supported on Unix systems (Linux, macOS)");
    std::process::exit(1);
}
