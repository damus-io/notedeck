//! CLI tool to spawn a new agent in a running notedeck instance.
//!
//! Usage:
//!   notedeck-spawn              # spawn with current directory
//!   notedeck-spawn /path/to/dir # spawn with specific directory

#[cfg(unix)]
fn main() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
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

    let socket_path = notedeck_dave::ipc::socket_path();

    // Connect to the running notedeck instance
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not connect to notedeck at {}", socket_path.display());
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("Is notedeck running? Start it first with `notedeck`");
            std::process::exit(1);
        }
    };

    // Send spawn request
    let request = serde_json::json!({
        "type": "spawn_agent",
        "cwd": cwd
    });

    if let Err(e) = writeln!(stream, "{}", request) {
        eprintln!("Failed to send request: {}", e);
        std::process::exit(1);
    }

    // Read response
    let mut reader = BufReader::new(&stream);
    let mut response = String::new();
    if let Err(e) = reader.read_line(&mut response) {
        eprintln!("Failed to read response: {}", e);
        std::process::exit(1);
    }

    // Parse and display response
    match serde_json::from_str::<serde_json::Value>(&response) {
        Ok(json) => {
            if json.get("status").and_then(|s| s.as_str()) == Some("ok") {
                if let Some(id) = json.get("session_id") {
                    println!("Agent spawned (session {})", id);
                } else {
                    println!("Agent spawned");
                }
            } else if let Some(msg) = json.get("message").and_then(|m| m.as_str()) {
                eprintln!("Error: {}", msg);
                std::process::exit(1);
            } else {
                eprintln!("Unknown response: {}", response);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Invalid response: {}", e);
            eprintln!("Raw: {}", response);
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("notedeck-spawn is only supported on Unix systems (Linux, macOS)");
    std::process::exit(1);
}
