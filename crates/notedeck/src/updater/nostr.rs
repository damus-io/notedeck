use nostrdb::{Filter, Ndb, Note, Transaction};
use tracing::{info, warn};

use super::ReleaseInfo;

/// NIP-82 Software Release event kind
const RELEASE_KIND: u64 = 30063;

/// NIP-82 Software Asset event kind
const ASSET_KIND: u64 = 3063;

/// The app identifier used in NIP-82 "i" tags
pub const APP_ID: &str = "io.damus.notedeck";

/// Default trusted release signing pubkey
/// TODO: Replace with the actual release signing pubkey before shipping
pub const DEFAULT_RELEASE_PUBKEY: [u8; 32] = [
    0x32, 0xe1, 0x82, 0x76, 0x35, 0x45, 0x0e, 0xbb, 0x3c, 0x5a, 0x7d, 0x12, 0xc1, 0xf8, 0xe7, 0xb2,
    0xb5, 0x14, 0x43, 0x9a, 0xc1, 0x0a, 0x67, 0xee, 0xf3, 0xd9, 0xfd, 0x9c, 0x5c, 0x68, 0xe2, 0x45,
];

/// Release channels supported by the NIP-82 zapstore convention
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReleaseChannel {
    #[default]
    Main,
    Beta,
    Nightly,
    Dev,
}

impl ReleaseChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
            Self::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "main" => Some(Self::Main),
            "beta" => Some(Self::Beta),
            "nightly" => Some(Self::Nightly),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    /// Parse a channel from the settings string, defaulting to Main
    pub fn from_setting(s: &str) -> Self {
        Self::parse(s).unwrap_or_default()
    }
}

/// Returns the expected asset name for the current platform/arch
pub fn target_asset_name() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "notedeck-x86_64-linux.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "notedeck-aarch64-linux.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "notedeck-x86_64-macos.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "notedeck-aarch64-macos.tar.gz"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "notedeck-x86_64-windows.zip"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "notedeck-aarch64-windows.zip"
    }
    #[cfg(target_os = "android")]
    {
        "notedeck.apk"
    }
}

/// Returns the platform tag value for the current platform/arch (NIP-82 "f" tag)
pub fn target_platform_tag() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-aarch64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(target_os = "android")]
    {
        "android-aarch64"
    }
}

/// Build nostrdb filters for NIP-82 release events and asset events from the given pubkey.
/// Returns two filters: one for kind 30063 (releases) and one for kind 3063 (assets).
pub fn release_filter(pubkey: &[u8; 32], channel: ReleaseChannel) -> Vec<Filter> {
    vec![
        // Kind 30063: Software Release events filtered by app id and channel
        Filter::new()
            .authors([pubkey])
            .kinds([RELEASE_KIND])
            .tags([APP_ID], 'i')
            .tags([channel.as_str()], 'c')
            .limit(10)
            .build(),
        // Kind 3063: Software Asset events (we need these to resolve release references)
        Filter::new()
            .authors([pubkey])
            .kinds([ASSET_KIND])
            .tags([APP_ID], 'i')
            .limit(50)
            .build(),
    ]
}

#[derive(Debug)]
pub enum ReleaseParseError {
    MissingTag(&'static str),
    WrongPlatform { got: String, expected: String },
    InvalidVersion(String),
    NotNewer { current: String, remote: String },
    AssetNotFound,
}

impl std::fmt::Display for ReleaseParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTag(tag) => write!(f, "missing '{tag}' tag"),
            Self::WrongPlatform { got, expected } => {
                write!(f, "wrong platform: got '{got}', expected '{expected}'")
            }
            Self::InvalidVersion(v) => write!(f, "invalid semver: '{v}'"),
            Self::NotNewer { current, remote } => {
                write!(f, "not newer: current={current}, remote={remote}")
            }
            Self::AssetNotFound => write!(f, "no matching asset event found"),
        }
    }
}

/// Parsed info from a kind 30063 release event (before asset resolution)
struct ReleaseEventInfo {
    version: String,
    asset_event_ids: Vec<[u8; 32]>,
}

/// Parse a NIP-82 kind 30063 (Software Release) event to extract version and asset references.
fn parse_release_event(note: &Note) -> Result<ReleaseEventInfo, ReleaseParseError> {
    let mut version = None;
    let mut asset_event_ids = Vec::new();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(key) = tag.get_str(0) else {
            continue;
        };

        match key {
            "version" => version = tag.get_str(1).map(|s| s.to_owned()),
            "e" => {
                if let Some(id) = tag.get_id(1) {
                    asset_event_ids.push(*id);
                }
            }
            _ => {}
        }
    }

    let version_str = version.ok_or(ReleaseParseError::MissingTag("version"))?;

    // Only return if newer than current version
    let current_version = env!("CARGO_PKG_VERSION");
    let current = semver::Version::parse(current_version)
        .map_err(|_| ReleaseParseError::InvalidVersion(current_version.to_string()))?;
    let remote = semver::Version::parse(&version_str)
        .map_err(|_| ReleaseParseError::InvalidVersion(version_str.clone()))?;

    if remote <= current {
        return Err(ReleaseParseError::NotNewer {
            current: current_version.to_string(),
            remote: version_str,
        });
    }

    if asset_event_ids.is_empty() {
        return Err(ReleaseParseError::MissingTag("e"));
    }

    Ok(ReleaseEventInfo {
        version: version_str,
        asset_event_ids,
    })
}

/// Parse a NIP-82 kind 3063 (Software Asset) event to extract download info.
/// Returns None if the asset doesn't match the current platform.
fn parse_asset_event(note: &Note) -> Option<ReleaseInfo> {
    let mut url = None;
    let mut sha256 = None;
    let mut version = None;
    let mut platform = None;
    let mut name = None;

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(key) = tag.get_str(0) else {
            continue;
        };

        match key {
            "url" => url = tag.get_str(1).map(|s| s.to_owned()),
            "x" => {
                sha256 = tag
                    .get_id(1)
                    .map(hex::encode)
                    .or_else(|| tag.get_str(1).map(|s| s.to_owned()));
            }
            "version" => version = tag.get_str(1).map(|s| s.to_owned()),
            "f" => platform = tag.get_str(1).map(|s| s.to_owned()),
            "name" => name = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    // Check platform match
    let expected_platform = target_platform_tag();
    let plat = platform.as_deref()?;
    if plat != expected_platform {
        return None;
    }

    let url = url?;
    let sha256 = sha256?;
    let version = version?;
    // Use name if present, otherwise derive from URL
    let asset_name =
        name.unwrap_or_else(|| url.rsplit('/').next().unwrap_or("notedeck").to_string());

    Some(ReleaseInfo {
        version,
        asset_url: url,
        asset_name,
        expected_sha256: sha256,
    })
}

/// Query ndb for the latest release matching our platform that is newer
/// than the currently running version.
///
/// Two-step lookup: find kind 30063 release events, then resolve their
/// "e" tag references to kind 3063 asset events for platform matching.
pub fn find_latest_release(
    ndb: &Ndb,
    txn: &Transaction,
    pubkey: &[u8; 32],
    channel: ReleaseChannel,
) -> Option<ReleaseInfo> {
    let filters = release_filter(pubkey, channel);

    let results = match ndb.query(txn, &filters, 50) {
        Ok(r) => r,
        Err(e) => {
            warn!("failed to query ndb for release events: {e}");
            return None;
        }
    };

    let mut best: Option<ReleaseInfo> = None;

    // Filter to only kind 30063 release events
    for result in &results {
        if result.note.kind() as u64 != RELEASE_KIND {
            continue;
        }

        let release_info = match parse_release_event(&result.note) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Check if this version is better than our current best
        let dominated = best.as_ref().is_some_and(|b| {
            semver::Version::parse(&release_info.version)
                .ok()
                .zip(semver::Version::parse(&b.version).ok())
                .is_some_and(|(new, old)| new <= old)
        });

        if dominated {
            continue;
        }

        // Resolve asset references — find a matching platform asset
        for asset_id in &release_info.asset_event_ids {
            let asset_note = ndb
                .get_notekey_by_id(txn, asset_id)
                .ok()
                .and_then(|nk| ndb.get_note_by_key(txn, nk).ok());

            let Some(asset_note) = asset_note else {
                continue;
            };

            if let Some(asset_info) = parse_asset_event(&asset_note) {
                info!("found release candidate: v{}", release_info.version);
                best = Some(asset_info);
                break;
            }
        }
    }

    best
}

/// Test helpers for constructing NIP-82 release events, available to other crates
/// when the `snapshot-testing` feature is enabled.
#[cfg(any(test, feature = "snapshot-testing"))]
pub mod test_helpers {
    use super::*;
    use nostrdb::NoteBuilder;

    /// A throwaway secret key for test signing (never used on a relay)
    pub const TEST_SECRET_KEY: [u8; 32] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01,
    ];

    /// The pubkey corresponding to TEST_SECRET_KEY
    pub const TEST_PUBKEY: [u8; 32] = [
        0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
        0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8,
        0x17, 0x98,
    ];

    /// Build a properly signed kind 3063 (Software Asset) event.
    /// Returns the event JSON string and the note id bytes.
    pub fn build_signed_asset_event(
        seckey: &[u8; 32],
        version: &str,
        platform: &str,
        url: &str,
        sha256: &str,
    ) -> (String, [u8; 32]) {
        let note = NoteBuilder::new()
            .kind(ASSET_KIND as u32)
            .content("")
            .sign(seckey)
            .start_tag()
            .tag_str("i")
            .tag_str(APP_ID)
            .start_tag()
            .tag_str("url")
            .tag_str(url)
            .start_tag()
            .tag_str("x")
            .tag_str(sha256)
            .start_tag()
            .tag_str("version")
            .tag_str(version)
            .start_tag()
            .tag_str("f")
            .tag_str(platform)
            .start_tag()
            .tag_str("m")
            .tag_str("application/gzip")
            .start_tag()
            .tag_str("size")
            .tag_str("12345678")
            .build()
            .expect("build asset note");

        let id = *note.id();
        let json = note.json().expect("serialize note to json");
        let event_str = format!(r#"["EVENT", "test_sub", {json}]"#);
        (event_str, id)
    }

    /// Build a properly signed kind 30063 (Software Release) event
    /// referencing the given asset event ids.
    pub fn build_signed_release_event(
        seckey: &[u8; 32],
        version: &str,
        channel: &str,
        asset_ids: &[[u8; 32]],
    ) -> String {
        let d_tag = format!("{APP_ID}@{version}");
        let mut builder = NoteBuilder::new()
            .kind(RELEASE_KIND as u32)
            .content("")
            .sign(seckey)
            .start_tag()
            .tag_str("d")
            .tag_str(&d_tag)
            .start_tag()
            .tag_str("i")
            .tag_str(APP_ID)
            .start_tag()
            .tag_str("version")
            .tag_str(version)
            .start_tag()
            .tag_str("c")
            .tag_str(channel);

        for asset_id in asset_ids {
            builder = builder.start_tag().tag_str("e").tag_id(asset_id);
        }

        let note = builder.build().expect("build release note");
        let json = note.json().expect("serialize note to json");
        format!(r#"["EVENT", "test_sub", {json}]"#)
    }

    /// Construct a kind 3063 (Software Asset) event JSON string for testing.
    /// Uses a dummy sig so `skip_validation` must be enabled on the ndb.
    pub fn make_asset_event_json(
        id: &str,
        pubkey: &str,
        version: &str,
        platform: &str,
        url: &str,
        sha256: &str,
        created_at: u64,
    ) -> String {
        format!(
            r#"["EVENT", "test_sub", {{
                "id": "{id}",
                "pubkey": "{pubkey}",
                "created_at": {created_at},
                "kind": {ASSET_KIND},
                "tags": [
                    ["i", "{APP_ID}"],
                    ["url", "{url}"],
                    ["x", "{sha256}"],
                    ["version", "{version}"],
                    ["f", "{platform}"],
                    ["m", "application/gzip"],
                    ["size", "12345678"]
                ],
                "content": "",
                "sig": "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
            }}]"#
        )
    }

    /// Construct a kind 30063 (Software Release) event JSON string for testing.
    /// Uses a dummy sig so `skip_validation` must be enabled on the ndb.
    pub fn make_release_event_json(
        id: &str,
        pubkey: &str,
        version: &str,
        channel: &str,
        asset_event_ids: &[&str],
        created_at: u64,
    ) -> String {
        let d_tag = format!("{APP_ID}@{version}");
        let mut e_tags = String::new();
        for asset_id in asset_event_ids {
            e_tags.push_str(&format!(r#",["e", "{asset_id}"]"#));
        }
        format!(
            r#"["EVENT", "test_sub", {{
                "id": "{id}",
                "pubkey": "{pubkey}",
                "created_at": {created_at},
                "kind": {RELEASE_KIND},
                "tags": [
                    ["d", "{d_tag}"],
                    ["i", "{APP_ID}"],
                    ["version", "{version}"],
                    ["c", "{channel}"]{e_tags}
                ],
                "content": "",
                "sig": "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
            }}]"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use nostrdb::{Config, IngestMetadata, Ndb};
    use tempfile::TempDir;

    /// Hex pubkey string for use with make_*_event_json (skip_validation tests)
    const TEST_PUBKEY_HEX: &str =
        "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().unwrap();
        let cfg = Config::new().skip_validation(true);
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &cfg).unwrap();
        (tmp, ndb)
    }

    #[test]
    fn test_target_asset_name_is_valid() {
        let name = target_asset_name();
        assert!(
            name.starts_with("notedeck-"),
            "asset name should start with 'notedeck-'"
        );
        assert!(
            name.ends_with(".tar.gz") || name.ends_with(".zip"),
            "asset name should end with .tar.gz or .zip"
        );
    }

    #[test]
    fn test_target_platform_tag_is_valid() {
        let tag = target_platform_tag();
        let parts: Vec<&str> = tag.split('-').collect();
        assert_eq!(parts.len(), 2, "platform tag should be os-arch: {tag}");
        assert!(
            ["linux", "macos", "windows"].contains(&parts[0]),
            "unexpected OS in platform tag: {tag}"
        );
        assert!(
            ["x86_64", "aarch64"].contains(&parts[1]),
            "unexpected arch in platform tag: {tag}"
        );
    }

    #[test]
    fn test_release_channel_roundtrip() {
        for ch in [
            ReleaseChannel::Main,
            ReleaseChannel::Beta,
            ReleaseChannel::Nightly,
            ReleaseChannel::Dev,
        ] {
            assert_eq!(ReleaseChannel::parse(ch.as_str()), Some(ch));
        }
        assert_eq!(ReleaseChannel::parse("unknown"), None);
    }

    #[test]
    fn test_release_filter_builds() {
        let filters = release_filter(&DEFAULT_RELEASE_PUBKEY, ReleaseChannel::Main);
        assert_eq!(filters.len(), 2, "should have release + asset filters");
    }

    #[tokio::test]
    async fn test_find_latest_release_nip82() {
        let (_tmp, ndb) = test_ndb();
        let platform = target_platform_tag();
        let expected_sha = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        // Asset event id (fake but deterministic)
        let asset_id = "aa00000000000000000000000000000000000000000000000000000000000001";

        // Create asset event (kind 3063)
        let asset_ev = make_asset_event_json(
            asset_id,
            TEST_PUBKEY_HEX,
            "99.0.0",
            platform,
            "https://example.com/download/notedeck.tar.gz",
            expected_sha,
            1700000000,
        );

        // Create release event (kind 30063) referencing the asset
        let release_ev = make_release_event_json(
            "bb00000000000000000000000000000000000000000000000000000000000001",
            TEST_PUBKEY_HEX,
            "99.0.0",
            "main",
            &[asset_id],
            1700000001,
        );

        // Subscribe and ingest both events
        let filter = Filter::new()
            .kinds([RELEASE_KIND, ASSET_KIND])
            .limit(20)
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&asset_ev, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev, IngestMetadata::new())
            .unwrap();

        let _ = ndb.wait_for_all_notes(sub, 2).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let release = find_latest_release(&ndb, &txn, &TEST_PUBKEY, ReleaseChannel::Main)
            .expect("should find a release");

        assert_eq!(release.version, "99.0.0");
        assert_eq!(release.expected_sha256, expected_sha);
        assert_eq!(
            release.asset_url,
            "https://example.com/download/notedeck.tar.gz"
        );
    }

    #[tokio::test]
    async fn test_find_latest_release_wrong_platform() {
        let (_tmp, ndb) = test_ndb();

        // Use a platform that doesn't match
        let wrong_platform = if target_platform_tag().contains("linux") {
            "windows-x86_64"
        } else {
            "linux-x86_64"
        };

        let asset_id = "cc00000000000000000000000000000000000000000000000000000000000001";

        let asset_ev = make_asset_event_json(
            asset_id,
            TEST_PUBKEY_HEX,
            "99.0.0",
            wrong_platform,
            "https://example.com/download/wrong.tar.gz",
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            1700000000,
        );

        let release_ev = make_release_event_json(
            "dd00000000000000000000000000000000000000000000000000000000000001",
            TEST_PUBKEY_HEX,
            "99.0.0",
            "main",
            &[asset_id],
            1700000001,
        );

        let filter = Filter::new()
            .kinds([RELEASE_KIND, ASSET_KIND])
            .limit(20)
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&asset_ev, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev, IngestMetadata::new())
            .unwrap();

        let _ = ndb.wait_for_all_notes(sub, 2).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let release = find_latest_release(&ndb, &txn, &TEST_PUBKEY, ReleaseChannel::Main);

        assert!(release.is_none(), "should not match wrong platform");
    }

    #[tokio::test]
    async fn test_find_latest_release_picks_highest_version() {
        let (_tmp, ndb) = test_ndb();
        let platform = target_platform_tag();

        // Two releases: v98 and v99
        let asset_id_98 = "ee00000000000000000000000000000000000000000000000000000000000001";
        let asset_id_99 = "ee00000000000000000000000000000000000000000000000000000000000002";

        let asset_ev_98 = make_asset_event_json(
            asset_id_98,
            TEST_PUBKEY_HEX,
            "98.0.0",
            platform,
            "https://example.com/download/v98.tar.gz",
            "aaaa000000000000000000000000000000000000000000000000000000000000",
            1700000000,
        );
        let asset_ev_99 = make_asset_event_json(
            asset_id_99,
            TEST_PUBKEY_HEX,
            "99.0.0",
            platform,
            "https://example.com/download/v99.tar.gz",
            "bbbb000000000000000000000000000000000000000000000000000000000000",
            1700000001,
        );

        let release_ev_98 = make_release_event_json(
            "ff00000000000000000000000000000000000000000000000000000000000001",
            TEST_PUBKEY_HEX,
            "98.0.0",
            "main",
            &[asset_id_98],
            1700000002,
        );
        let release_ev_99 = make_release_event_json(
            "ff00000000000000000000000000000000000000000000000000000000000002",
            TEST_PUBKEY_HEX,
            "99.0.0",
            "main",
            &[asset_id_99],
            1700000003,
        );

        let filter = Filter::new()
            .kinds([RELEASE_KIND, ASSET_KIND])
            .limit(20)
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&asset_ev_98, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&asset_ev_99, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev_98, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev_99, IngestMetadata::new())
            .unwrap();

        let _ = ndb.wait_for_all_notes(sub, 4).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let release = find_latest_release(&ndb, &txn, &TEST_PUBKEY, ReleaseChannel::Main)
            .expect("should find a release");

        assert_eq!(release.version, "99.0.0");
        assert_eq!(
            release.expected_sha256,
            "bbbb000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[tokio::test]
    async fn test_find_latest_release_older_version() {
        let (_tmp, ndb) = test_ndb();
        let platform = target_platform_tag();

        let asset_id = "1100000000000000000000000000000000000000000000000000000000000001";

        let asset_ev = make_asset_event_json(
            asset_id,
            TEST_PUBKEY_HEX,
            "0.0.1",
            platform,
            "https://example.com/download/old.tar.gz",
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            1700000000,
        );

        let release_ev = make_release_event_json(
            "2200000000000000000000000000000000000000000000000000000000000001",
            TEST_PUBKEY_HEX,
            "0.0.1",
            "main",
            &[asset_id],
            1700000001,
        );

        let filter = Filter::new()
            .kinds([RELEASE_KIND, ASSET_KIND])
            .limit(20)
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&asset_ev, IngestMetadata::new())
            .unwrap();
        ndb.process_event_with(&release_ev, IngestMetadata::new())
            .unwrap();

        let _ = ndb.wait_for_all_notes(sub, 2).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let release = find_latest_release(&ndb, &txn, &TEST_PUBKEY, ReleaseChannel::Main);

        assert!(
            release.is_none(),
            "should not return older version than current"
        );
    }
}
