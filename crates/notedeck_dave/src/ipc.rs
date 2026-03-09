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
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;
    use std::thread;

    /// A pending IPC connection that needs to be processed
    pub struct PendingConnection {
        pub stream: UnixStream,
        pub cwd: PathBuf,
    }

    /// Handle to the IPC listener background thread
    pub struct IpcListener {
        receiver: mpsc::Receiver<PendingConnection>,
    }

    impl IpcListener {
        /// Poll for pending connections (non-blocking)
        pub fn try_recv(&self) -> Option<PendingConnection> {
            self.receiver.try_recv().ok()
        }
    }

    /// Creates an IPC listener that runs in a background thread.
    ///
    /// The background thread blocks on accept() and calls request_repaint()
    /// when a connection arrives, ensuring the UI wakes up immediately.
    ///
    /// Returns None if the socket cannot be created (e.g., permission issues).
    pub fn create_listener(ctx: egui::Context) -> Option<IpcListener> {
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

        // Create and bind the listener (blocking mode for the background thread)
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => {
                tracing::info!("IPC listener started at {}", path.display());
                listener
            }
            Err(e) => {
                tracing::warn!("Failed to create IPC listener: {}", e);
                return None;
            }
        };

        // Channel for sending connections to the main thread
        let (sender, receiver) = mpsc::channel();

        // Spawn background thread to handle incoming connections
        thread::Builder::new()
            .name("ipc-listener".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(mut stream) => {
                            // Parse the request in the background thread
                            match handle_connection(&mut stream) {
                                Ok(cwd) => {
                                    let pending = PendingConnection { stream, cwd };
                                    if sender.send(pending).is_err() {
                                        // Main thread dropped the receiver, exit
                                        tracing::debug!("IPC listener: main thread gone, exiting");
                                        break;
                                    }
                                    // Wake up the UI to process the connection
                                    ctx.request_repaint();
                                }
                                Err(e) => {
                                    // Send error response directly
                                    let response = SpawnResponse::error(&e);
                                    let _ = send_response(&mut stream, &response);
                                    tracing::warn!("IPC spawn-agent failed: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("IPC accept error: {}", e);
                        }
                    }
                }
                tracing::debug!("IPC listener thread exiting");
            })
            .ok()?;

        Some(IpcListener { receiver })
    }

    /// Handles a single IPC connection, returning the cwd if valid spawn request.
    pub fn handle_connection(stream: &mut UnixStream) -> Result<PathBuf, String> {
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
    pub fn send_response(stream: &mut UnixStream, response: &SpawnResponse) -> std::io::Result<()> {
        let json = serde_json::to_string(response)?;
        writeln!(stream, "{}", json)?;
        stream.flush()
    }
}

// Stub for non-Unix platforms (Windows)
#[cfg(not(unix))]
pub mod non_unix {
    use std::path::PathBuf;

    /// Stub for PendingConnection on non-Unix platforms
    pub struct PendingConnection {
        pub cwd: PathBuf,
    }

    /// Stub for IpcListener on non-Unix platforms
    pub struct IpcListener;

    impl IpcListener {
        pub fn try_recv(&self) -> Option<PendingConnection> {
            None
        }
    }

    pub fn create_listener(_ctx: egui::Context) -> Option<IpcListener> {
        tracing::info!("IPC spawn-agent not supported on this platform");
        None
    }
}

#[cfg(not(unix))]
pub use non_unix::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_request_roundtrip() {
        let req = SpawnRequest {
            request_type: "spawn_agent".to_string(),
            cwd: PathBuf::from("/home/user/project"),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SpawnRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.request_type, "spawn_agent");
        assert_eq!(parsed.cwd, PathBuf::from("/home/user/project"));
    }

    #[test]
    fn spawn_request_type_field_renamed() {
        // Verify the "type" field rename works in JSON
        let json = r#"{"type":"spawn_agent","cwd":"/tmp"}"#;
        let req: SpawnRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.request_type, "spawn_agent");
    }

    #[test]
    fn spawn_response_ok() {
        let resp = SpawnResponse::ok(42);
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.session_id, Some(42));
        assert!(resp.message.is_none());
    }

    #[test]
    fn spawn_response_error() {
        let resp = SpawnResponse::error("something went wrong");
        assert_eq!(resp.status, "error");
        assert!(resp.session_id.is_none());
        assert_eq!(resp.message.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn spawn_response_ok_serialization() {
        let resp = SpawnResponse::ok(7);
        let json = serde_json::to_string(&resp).unwrap();
        // session_id should be present, message should be absent (skip_serializing_if)
        assert!(json.contains("\"session_id\":7"));
        assert!(!json.contains("message"));
    }

    #[test]
    fn spawn_response_error_serialization() {
        let resp = SpawnResponse::error("bad");
        let json = serde_json::to_string(&resp).unwrap();
        // message should be present, session_id should be absent
        assert!(json.contains("\"message\":\"bad\""));
        assert!(!json.contains("session_id"));
    }

    #[test]
    fn socket_path_ends_with_spawn_sock() {
        let path = socket_path();
        assert!(
            path.ends_with("notedeck/spawn.sock"),
            "socket path should end with notedeck/spawn.sock, got: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn handle_connection_valid_spawn() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        // Write a valid spawn request
        let req = SpawnRequest {
            request_type: "spawn_agent".to_string(),
            cwd: PathBuf::from("/tmp"),
        };
        let json = serde_json::to_string(&req).unwrap();
        writeln!(client, "{}", json).unwrap();
        // Shutdown write side so BufReader sees EOF after the line
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let result = handle_connection(&mut server);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), PathBuf::from("/tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_connection_invalid_type() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        let json = r#"{"type":"unknown_command","cwd":"/tmp"}"#;
        writeln!(client, "{}", json).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let result = handle_connection(&mut server);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown request type"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_connection_invalid_json() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        writeln!(client, "not valid json at all").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let result = handle_connection(&mut server);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_connection_nonexistent_path() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        let json = r#"{"type":"spawn_agent","cwd":"/nonexistent/path/that/does/not/exist"}"#;
        writeln!(client, "{}", json).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let result = handle_connection(&mut server);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_connection_path_is_file_not_directory() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let temp = tempfile::NamedTempFile::new().unwrap();
        let file_path = temp.path().to_string_lossy().to_string();

        let (mut client, mut server) = UnixStream::pair().unwrap();
        let json = format!(r#"{{"type":"spawn_agent","cwd":"{}"}}"#, file_path);
        writeln!(client, "{}", json).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let result = handle_connection(&mut server);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("not a directory"),
            "should reject file path as cwd"
        );
    }
}
