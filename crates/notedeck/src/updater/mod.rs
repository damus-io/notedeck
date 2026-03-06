mod github;
mod platform;

use crate::{DataPath, DataPathType};
use std::path::PathBuf;
use std::sync::mpsc;
use tracing::{error, info, warn};

pub use github::ReleaseInfo;

/// Messages sent from background tasks to the Updater
enum UpdateMsg {
    /// Result of checking GitHub for updates
    CheckResult(Result<Option<ReleaseInfo>, String>),
    /// Download completed
    DownloadComplete(Result<PathBuf, String>),
}

/// The current state of the auto-updater
enum UpdateState {
    /// Haven't started checking yet
    Idle,
    /// Waiting for GitHub API response
    Checking,
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

/// Auto-updater that checks GitHub Releases for new versions,
/// downloads updates in the background, and prompts the user
/// to restart.
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
                UpdateMsg::CheckResult(result) => self.handle_check_result(result),
                UpdateMsg::DownloadComplete(result) => self.handle_download_complete(result),
            }
        }

        // Auto-transition from Idle to Checking
        if matches!(self.state, UpdateState::Idle) {
            self.start_check();
        }
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

    fn start_check(&mut self) {
        info!("checking for updates...");
        self.state = UpdateState::Checking;

        let tx = self.tx.clone();
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let ctx = self.ctx.clone();

        github::check_for_update(&current_version, move |result| {
            let _ = tx.send(UpdateMsg::CheckResult(result));
            ctx.request_repaint();
        });
    }

    fn handle_check_result(&mut self, result: Result<Option<ReleaseInfo>, String>) {
        match result {
            Ok(Some(release)) => {
                info!("update available: v{}", release.version);
                self.start_download(release);
            }
            Ok(None) => {
                info!("already up to date");
                self.state = UpdateState::UpToDate;
            }
            Err(e) => {
                warn!("update check failed: {e}");
                self.state = UpdateState::Error(e);
            }
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

        // Download the asset
        let mut request = ehttp::Request::get(&release.asset_url);
        request
            .headers
            .insert("User-Agent".to_string(), "notedeck-updater".to_string());

        info!("downloading update: {}", release.asset_url);

        ehttp::fetch(request, move |response| {
            let result = handle_download(response, &release.asset_name, &staging_dir);
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

/// Handle a completed download: save to disk, extract, find binary
fn handle_download(
    response: Result<ehttp::Response, String>,
    asset_name: &str,
    staging_dir: &Path,
) -> Result<PathBuf, String> {
    let response = response.map_err(|e| format!("Download failed: {e}"))?;

    if response.status != 200 {
        return Err(format!("Download returned status {}", response.status));
    }

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

use std::path::Path;
