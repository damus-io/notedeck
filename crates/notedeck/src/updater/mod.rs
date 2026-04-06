pub mod nostr;
pub mod platform;

use crate::{DataPath, DataPathType};
use nostrdb::{Ndb, Subscription};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};

/// How long to wait after receiving the first release event before
/// picking the best version. Gives slower relays time to respond.
const GATHER_DEBOUNCE: Duration = Duration::from_secs(3);

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
    /// Got at least one release event, waiting for more relays to respond
    GatheringReleases { deadline: Instant },
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
    sent_relay_filter: bool,
    release_pubkey: [u8; 32],
    channel: nostr::ReleaseChannel,
    release_sub: Subscription,
}

impl Updater {
    /// Create a new updater. Begins in `Idle` state.
    pub fn new(
        data_path: &DataPath,
        ndb: &Ndb,
        ctx: &egui::Context,
        release_pubkey: [u8; 32],
        channel: nostr::ReleaseChannel,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let staging_dir = data_path.path(DataPathType::Update);
        let _ = std::fs::create_dir_all(&staging_dir);
        let filters = nostr::release_filter(&release_pubkey);
        let release_sub = ndb.subscribe(&filters).expect("release subscription");

        Self {
            state: UpdateState::Idle,
            rx,
            tx,
            staging_dir,
            ctx: ctx.clone(),
            sent_relay_filter: false,
            release_pubkey,
            channel,
            release_sub,
        }
    }

    /// The trusted release signing pubkey
    pub fn release_pubkey(&self) -> &[u8; 32] {
        &self.release_pubkey
    }

    /// The current release channel
    pub fn channel(&self) -> nostr::ReleaseChannel {
        self.channel
    }

    /// The ndb subscription for release events
    pub fn release_sub(&self) -> Subscription {
        self.release_sub
    }

    /// Poll for state changes. Call this every frame from `eframe::App::update()`.
    /// This is non-blocking — it only does `try_recv()` on the channel.
    pub fn poll(&mut self, ndb: &Ndb) {
        // Process any messages from background tasks
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UpdateMsg::DownloadComplete(result) => self.handle_download_complete(result),
            }
        }

        // Auto-transition from Idle to WaitingForRelease.
        // Also check ndb for releases that were already ingested in a
        // previous session so we don't have to wait for relays again.
        if matches!(self.state, UpdateState::Idle) {
            info!(
                "updater: waiting for release events from nostr (channel={}, pkg_version=v{})",
                self.channel.as_str(),
                env!("CARGO_PKG_VERSION"),
            );
            self.state = UpdateState::WaitingForRelease;

            // Check for releases already in ndb from a prior session
            if let Ok(txn) = nostrdb::Transaction::new(ndb) {
                if let Some(release) =
                    nostr::find_latest_release(ndb, &txn, &self.release_pubkey, self.channel)
                {
                    info!(
                        "updater: found cached release v{}, starting download",
                        release.version
                    );
                    self.start_download(release);
                }
            }
        }
    }

    /// Whether the updater is listening for release events
    pub fn wants_release(&self) -> bool {
        matches!(
            self.state,
            UpdateState::WaitingForRelease | UpdateState::GatheringReleases { .. }
        )
    }

    /// Whether the release filter needs to be sent to remote relays.
    /// Returns true only once — after calling this, subsequent calls return false.
    pub fn needs_relay_sub(&mut self) -> bool {
        if self.sent_relay_filter {
            return false;
        }
        self.sent_relay_filter = true;
        true
    }

    /// Signal that new release events have arrived. Starts or resets the
    /// debounce timer so slower relays have time to deliver their events.
    pub fn note_received(&mut self) {
        match self.state {
            UpdateState::WaitingForRelease | UpdateState::GatheringReleases { .. } => {
                debug!("updater: release note received, starting/resetting debounce timer");
                self.state = UpdateState::GatheringReleases {
                    deadline: Instant::now() + GATHER_DEBOUNCE,
                };
            }
            _ => {
                debug!(
                    "updater: note_received called but state is not waiting/gathering, ignoring"
                );
            }
        }
    }

    /// Check if the gathering debounce has expired. If so, query ndb for
    /// the best release and start downloading. Call this every frame.
    pub fn check_gathering(&mut self, ndb: &Ndb) {
        let deadline = match self.state {
            UpdateState::GatheringReleases { deadline } => deadline,
            _ => return,
        };

        if Instant::now() < deadline {
            return;
        }

        let channel = self.channel;
        if let Ok(txn) = nostrdb::Transaction::new(ndb) {
            if let Some(release) =
                nostr::find_latest_release(ndb, &txn, &self.release_pubkey, channel)
            {
                info!("update available: v{}", release.version);
                self.start_download(release);
                return;
            }
        }

        // No valid release found after gathering — go back to waiting
        info!("updater: no matching release found after debounce, continuing to wait");
        self.state = UpdateState::WaitingForRelease;
    }

    /// Returns the new version string if an update is ready to install.
    pub fn update_ready(&self) -> Option<&str> {
        match &self.state {
            UpdateState::ReadyToInstall { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Returns the version being downloaded, if any.
    pub fn downloading_version(&self) -> Option<&str> {
        match &self.state {
            UpdateState::Downloading { version } => Some(version),
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

    /// Unsubscribe from the current release filter and resubscribe with
    /// the current pubkey.
    #[cfg(feature = "snapshot-testing")]
    fn resubscribe(&mut self, ndb: &mut Ndb) {
        let _ = ndb.unsubscribe(self.release_sub);
        let filters = nostr::release_filter(&self.release_pubkey);
        self.release_sub = ndb.subscribe(&filters).expect("release subscription");
        self.sent_relay_filter = false;
    }

    /// Change the release channel. Since channel filtering is done client-side
    /// via semver, no resubscription is needed — just reset state to re-evaluate.
    pub fn set_channel(&mut self, channel: nostr::ReleaseChannel) {
        if self.channel == channel {
            return;
        }
        self.channel = channel;
        // Reset to re-check with the new channel acceptance criteria
        if matches!(
            self.state,
            UpdateState::UpToDate
                | UpdateState::WaitingForRelease
                | UpdateState::GatheringReleases { .. }
        ) {
            self.state = UpdateState::Idle;
        }
    }

    /// Override the release signing pubkey and resubscribe (for tests)
    #[cfg(feature = "snapshot-testing")]
    pub fn set_release_pubkey(&mut self, ndb: &mut Ndb, pubkey: [u8; 32]) {
        self.release_pubkey = pubkey;
        self.resubscribe(ndb);
    }

    /// Force the updater into ReadyToInstall state (for snapshot tests)
    #[cfg(feature = "snapshot-testing")]
    pub fn force_ready(&mut self, version: String) {
        self.state = UpdateState::ReadyToInstall {
            version,
            binary_path: PathBuf::from("/dev/null"),
        };
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

    // Clean up the archive (but not if the binary IS the archive, e.g. APK)
    if binary_path != archive_path {
        let _ = std::fs::remove_file(&archive_path);
    }

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
    fn test_apk_download_not_deleted() {
        let fake_apk = b"PK\x03\x04fake apk content";
        let expected_sha = sha256_hex(fake_apk);

        let response = Ok(ehttp::Response {
            status: 200,
            status_text: "OK".to_string(),
            url: "https://example.com/notedeck.apk".to_string(),
            bytes: fake_apk.to_vec(),
            headers: Default::default(),
            ok: true,
        });

        let staging = tempfile::tempdir().unwrap();
        let result = handle_download(response, "notedeck.apk", &expected_sha, staging.path());

        assert!(result.is_ok(), "handle_download failed: {:?}", result.err());
        let apk_path = result.unwrap();
        assert!(
            apk_path.exists(),
            "APK should still exist after handle_download: {}",
            apk_path.display()
        );
        assert_eq!(std::fs::read(&apk_path).unwrap(), fake_apk);
    }

    /// Simulates a second app launch: release events are already in ndb
    /// from a prior session. The updater should discover them on its first
    /// poll() without waiting for new notes from relays.
    #[tokio::test]
    async fn test_poll_finds_cached_release() {
        use self::nostr::test_helpers::*;
        use crate::test_util::test_config;

        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config().skip_validation(true);
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &cfg).unwrap();

        let platform = nostr::target_platform_tag();
        let asset_id = "aa00000000000000000000000000000000000000000000000000000000000001";
        let pubkey_hex = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

        let asset_ev = make_asset_event_json(
            asset_id,
            pubkey_hex,
            "99.0.0",
            platform,
            "https://example.com/notedeck.tar.gz",
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            1700000000,
        );
        let release_ev = make_release_event_json(
            "bb00000000000000000000000000000000000000000000000000000000000001",
            pubkey_hex,
            "99.0.0",
            "main",
            &[asset_id],
            1700000001,
        );

        // Ingest events (simulating a prior session)
        let filter = nostrdb::Filter::new()
            .kinds([30063, 3063])
            .limit(20)
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&asset_ev, nostrdb::IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev, nostrdb::IngestMetadata::new())
            .unwrap();
        let _ = ndb.wait_for_all_notes(sub, 2).await.unwrap();

        // Now create a fresh Updater (simulating a new app launch)
        let data_path = DataPath::new(tmp.path());
        let ctx = egui::Context::default();
        let mut updater = Updater::new(
            &data_path,
            &ndb,
            &ctx,
            TEST_PUBKEY,
            nostr::ReleaseChannel::Main,
        );

        // First poll should find the cached release and start downloading
        updater.poll(&ndb);

        assert_eq!(
            updater.downloading_version(),
            Some("99.0.0"),
            "updater should start downloading cached release on first poll"
        );
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
