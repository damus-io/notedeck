//! IPC module for external spawn-agent commands via Unix domain sockets.
//!
//! This allows external tools (like `notedeck-spawn`) to create new agent
//! sessions in a running notedeck instance.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request to spawn a new agent
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnRequest {
    #[serde(rename = "type")]
    pub request_type: String,
    pub cwd: PathBuf,
}

/// Response to a spawn request
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SpawnResponse {
    pub fn ok(session_id: u32) -> Self {
        Self {
            status: "ok".to_string(),
            session_id: Some(session_id),
            message: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            session_id: None,
            message: Some(message.into()),
        }
    }
}

/// Returns the path to the IPC socket.
///
/// Uses XDG_RUNTIME_DIR on Linux (e.g., /run/user/1000/notedeck/spawn.sock)
/// or falls back to a user-local directory.
pub fn socket_path() -> PathBuf {
    // Try XDG_RUNTIME_DIR first (Linux)
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir)
            .join("notedeck")
            .join("spawn.sock");
    }

    // macOS: use Application Support
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        return home
            .join("Library")
            .join("Application Support")
            .join("notedeck")
            .join("spawn.sock");
    }

    // Fallback: ~/.local/share/notedeck
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("notedeck")
        .join("spawn.sock")
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    /// Creates a non-blocking Unix domain socket listener.
    ///
    /// Returns None if the socket cannot be created (e.g., permission issues).
    /// The socket file is removed if it already exists (stale from crash).
    pub fn create_listener() -> Option<UnixListener> {
        let path = socket_path();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("Failed to create IPC socket directory: {}", e);
                return None;
            }
        }

        // Remove stale socket if it exists
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("Failed to remove stale socket: {}", e);
                return None;
            }
        }

        // Create and bind the listener
        match UnixListener::bind(&path) {
            Ok(listener) => {
                // Set non-blocking for polling in event loop
                if let Err(e) = listener.set_nonblocking(true) {
                    tracing::warn!("Failed to set socket non-blocking: {}", e);
                    return None;
                }
                tracing::info!("IPC listener started at {}", path.display());
                Some(listener)
            }
            Err(e) => {
                tracing::warn!("Failed to create IPC listener: {}", e);
                None
            }
        }
    }

    /// Handles a single IPC connection, returning the cwd if valid spawn request.
    pub fn handle_connection(
        stream: &mut std::os::unix::net::UnixStream,
    ) -> Result<PathBuf, String> {
        // Read the request line
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;

        // Parse JSON request
        let request: SpawnRequest =
            serde_json::from_str(&line).map_err(|e| format!("Invalid JSON: {}", e))?;

        // Validate request type
        if request.request_type != "spawn_agent" {
            return Err(format!("Unknown request type: {}", request.request_type));
        }

        // Validate path exists and is a directory
        if !request.cwd.exists() {
            return Err(format!("Path does not exist: {}", request.cwd.display()));
        }
        if !request.cwd.is_dir() {
            return Err(format!(
                "Path is not a directory: {}",
                request.cwd.display()
            ));
        }

        Ok(request.cwd)
    }

    /// Sends a response back to the client
    pub fn send_response(
        stream: &mut std::os::unix::net::UnixStream,
        response: &SpawnResponse,
    ) -> std::io::Result<()> {
        let json = serde_json::to_string(response)?;
        writeln!(stream, "{}", json)?;
        stream.flush()
    }
}

// Stub for non-Unix platforms (Windows)
#[cfg(not(unix))]
pub fn create_listener() -> Option<()> {
    tracing::info!("IPC spawn-agent not supported on this platform");
    None
}
