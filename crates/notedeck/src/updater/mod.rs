pub mod nostr;
mod platform;

use crate::{DataPath, DataPathType};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use tracing::{error, info};

/// Information about a release asset available for download
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub asset_url: String,
    pub asset_name: String,
    pub expected_sha256: String,
}

/// Messages sent from background tasks to the Updater
enum UpdateMsg {
    /// Download completed
    DownloadComplete(Result<PathBuf, String>),
}

/// The current state of the auto-updater
enum UpdateState {
    /// Haven't started checking yet
    Idle,
    /// Waiting for a release event from ndb
    WaitingForRelease,
    /// Downloading the update archive
    Downloading { version: String },
    /// Downloaded and ready to install
    ReadyToInstall {
        version: String,
        binary_path: PathBuf,
    },
    /// Already up to date or user dismissed
    UpToDate,
    /// Something went wrong (non-fatal)
    #[allow(dead_code)]
    Error(String),
}

/// Auto-updater that discovers releases via Nostr events,
/// downloads updates in the background, verifies SHA256 hashes,
/// and prompts the user to restart.
pub struct Updater {
    state: UpdateState,
    rx: mpsc::Receiver<UpdateMsg>,
    tx: mpsc::Sender<UpdateMsg>,
    staging_dir: PathBuf,
    ctx: egui::Context,
}

impl Updater {
    /// Create a new updater. Begins in `Idle` state.
    pub fn new(data_path: &DataPath, ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let staging_dir = data_path.path(DataPathType::Update);
        let _ = std::fs::create_dir_all(&staging_dir);

        Self {
            state: UpdateState::Idle,
            rx,
            tx,
            staging_dir,
            ctx: ctx.clone(),
        }
    }

    /// Poll for state changes. Call this every frame from `eframe::App::update()`.
    /// This is non-blocking — it only does `try_recv()` on the channel.
    pub fn poll(&mut self) {
        // Process any messages from background tasks
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UpdateMsg::DownloadComplete(result) => self.handle_download_complete(result),
            }
        }

        // Auto-transition from Idle to WaitingForRelease
        if matches!(self.state, UpdateState::Idle) {
            info!("updater: waiting for release events from nostr...");
            self.state = UpdateState::WaitingForRelease;
        }
    }

    /// Whether the updater is waiting for a release to be provided
    pub fn wants_release(&self) -> bool {
        matches!(self.state, UpdateState::WaitingForRelease)
    }

    /// Provide a verified release (from a signed Nostr event) to begin downloading
    pub fn provide_release(&mut self, release: ReleaseInfo) {
        if !self.wants_release() {
            return;
        }

        info!("update available: v{}", release.version);
        self.start_download(release);
    }

    /// Returns the new version string if an update is ready to install.
    pub fn update_ready(&self) -> Option<&str> {
        match &self.state {
            UpdateState::ReadyToInstall { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Apply the staged update and restart the application.
    /// This function does not return on success (the process exits).
    pub fn apply_and_restart(&self) -> Result<(), String> {
        match &self.state {
            UpdateState::ReadyToInstall {
                binary_path,
                version,
                ..
            } => {
                info!("applying update to version {version}");
                platform::install_and_restart(binary_path)
            }
            _ => Err("No update ready to install".to_string()),
        }
    }

    /// User dismissed the update notification
    pub fn dismiss(&mut self) {
        if matches!(self.state, UpdateState::ReadyToInstall { .. }) {
            self.state = UpdateState::UpToDate;
        }
    }

    fn start_download(&mut self, release: ReleaseInfo) {
        let version = release.version.clone();
        self.state = UpdateState::Downloading {
            version: version.clone(),
        };

        let tx = self.tx.clone();
        let staging_dir = self.staging_dir.clone();
        let ctx = self.ctx.clone();

        let mut request = ehttp::Request::get(&release.asset_url);
        request
            .headers
            .insert("User-Agent".to_string(), "notedeck-updater".to_string());

        info!("downloading update: {}", release.asset_url);

        ehttp::fetch(request, move |response| {
            let result = handle_download(
                response,
                &release.asset_name,
                &release.expected_sha256,
                &staging_dir,
            );
            let _ = tx.send(UpdateMsg::DownloadComplete(result));
            ctx.request_repaint();
        });
    }

    fn handle_download_complete(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(binary_path) => {
                let version = match &self.state {
                    UpdateState::Downloading { version } => version.clone(),
                    _ => "unknown".to_string(),
                };
                info!(
                    "update downloaded and extracted to '{}'",
                    binary_path.display()
                );
                self.state = UpdateState::ReadyToInstall {
                    version,
                    binary_path,
                };
            }
            Err(e) => {
                error!("download failed: {e}");
                self.state = UpdateState::Error(e);
            }
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Handle a completed download: verify SHA256, save to disk, extract, find binary
fn handle_download(
    response: Result<ehttp::Response, String>,
    asset_name: &str,
    expected_sha256: &str,
    staging_dir: &Path,
) -> Result<PathBuf, String> {
    let response = response.map_err(|e| format!("Download failed: {e}"))?;

    if response.status != 200 {
        return Err(format!("Download returned status {}", response.status));
    }

    // Verify SHA256 before writing to disk
    let actual_sha256 = sha256_hex(&response.bytes);
    if actual_sha256 != expected_sha256 {
        return Err(format!(
            "SHA256 mismatch: expected {expected_sha256}, got {actual_sha256}"
        ));
    }

    info!("SHA256 verified: {actual_sha256}");

    // Save archive to staging dir
    let archive_path = staging_dir.join(asset_name);
    std::fs::write(&archive_path, &response.bytes)
        .map_err(|e| format!("Failed to write archive: {e}"))?;

    info!(
        "saved {} bytes to '{}'",
        response.bytes.len(),
        archive_path.display()
    );

    // Extract and find the binary
    let binary_path = platform::extract_archive(&archive_path, staging_dir)?;

    // Clean up the archive
    let _ = std::fs::remove_file(&archive_path);

    Ok(binary_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        // SHA256 of empty string
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_hex_known_input() {
        let hash = sha256_hex(b"hello world");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_sha256_verification_mismatch() {
        let response = Ok(ehttp::Response {
            status: 200,
            status_text: "OK".to_string(),
            url: "https://example.com/test.tar.gz".to_string(),
            bytes: b"some file content".to_vec(),
            headers: Default::default(),
            ok: true,
        });

        let staging = tempfile::tempdir().unwrap();
        let result = handle_download(
            response,
            "test.tar.gz",
            "0000000000000000000000000000000000000000000000000000000000000000",
            staging.path(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("SHA256 mismatch"),
            "error should mention SHA256 mismatch: {err}"
        );
    }

    #[test]
    fn test_sha256_verification_http_error() {
        let response = Err("connection refused".to_string());

        let staging = tempfile::tempdir().unwrap();
        let result = handle_download(response, "test.tar.gz", "doesntmatter", staging.path());

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Download failed"));
    }

    #[test]
    fn test_sha256_verification_bad_status() {
        let response = Ok(ehttp::Response {
            status: 404,
            status_text: "Not Found".to_string(),
            url: "https://example.com/test.tar.gz".to_string(),
            bytes: vec![],
            headers: Default::default(),
            ok: false,
        });

        let staging = tempfile::tempdir().unwrap();
        let result = handle_download(response, "test.tar.gz", "doesntmatter", staging.path());

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("status 404"));
    }
}
