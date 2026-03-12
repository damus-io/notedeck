use nostrdb::{Filter, Ndb, Note, Transaction};
use tracing::{info, warn};

use super::ReleaseInfo;

/// The kind for NIP-94 file metadata events (used by zapstore convention)
const RELEASE_KIND: u64 = 1063;

/// Trusted release signing pubkey (hex-encoded, set during build/release configuration)
/// TODO: Replace with the actual release signing pubkey before shipping
const RELEASE_PUBKEY_HEX: &str = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245";

fn release_pubkey() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(RELEASE_PUBKEY_HEX, &mut bytes).expect("RELEASE_PUBKEY_HEX is valid hex");
    bytes
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
}

/// Build a nostrdb filter for release file metadata events from the trusted pubkey
pub fn release_filter() -> Vec<Filter> {
    let pk = release_pubkey();
    vec![Filter::new()
        .authors([&pk])
        .kinds([RELEASE_KIND])
        .limit(10)
        .build()]
}

#[derive(Debug)]
pub enum ReleaseParseError {
    MissingTag(&'static str),
    WrongPlatform { got: String, expected: String },
    InvalidVersion(String),
    NotNewer { current: String, remote: String },
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
        }
    }
}

/// Parse a NIP-94 file metadata note into a ReleaseInfo, if it matches
/// the current platform and is newer than the running version.
pub fn parse_release_note(note: &Note) -> Result<ReleaseInfo, ReleaseParseError> {
    let mut url = None;
    let mut sha256 = None;
    let mut version = None;
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
                // nostrdb stores 32-byte hex strings as binary internally
                sha256 = tag
                    .get_id(1)
                    .map(hex::encode)
                    .or_else(|| tag.get_str(1).map(|s| s.to_owned()));
            }
            "version" => version = tag.get_str(1).map(|s| s.to_owned()),
            "name" => name = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    let url = url.ok_or(ReleaseParseError::MissingTag("url"))?;
    let sha256 = sha256.ok_or(ReleaseParseError::MissingTag("x"))?;
    let version_str = version.ok_or(ReleaseParseError::MissingTag("version"))?;
    let asset_name = name.ok_or(ReleaseParseError::MissingTag("name"))?;

    // Only match events for our platform
    let expected = target_asset_name();
    if asset_name != expected {
        return Err(ReleaseParseError::WrongPlatform {
            got: asset_name,
            expected: expected.to_string(),
        });
    }

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

    Ok(ReleaseInfo {
        version: version_str,
        asset_url: url,
        asset_name,
        expected_sha256: sha256,
    })
}

/// Query ndb for the latest release matching our platform that is newer
/// than the currently running version.
pub fn find_latest_release(ndb: &Ndb, txn: &Transaction) -> Option<ReleaseInfo> {
    let filters = release_filter();

    let results = match ndb.query(txn, &filters, 10) {
        Ok(r) => r,
        Err(e) => {
            warn!("failed to query ndb for release events: {e}");
            return None;
        }
    };

    let mut best: Option<ReleaseInfo> = None;

    for result in results {
        if let Ok(release) = parse_release_note(&result.note) {
            let dominated = best.as_ref().is_some_and(|b| {
                semver::Version::parse(&release.version)
                    .ok()
                    .zip(semver::Version::parse(&b.version).ok())
                    .is_some_and(|(new, old)| new <= old)
            });

            if !dominated {
                info!("found release candidate: v{}", release.version);
                best = Some(release);
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::{Config, IngestMetadata, Ndb};
    use tempfile::TempDir;

    fn make_release_event_json(
        pubkey: &str,
        version: &str,
        asset_name: &str,
        url: &str,
        sha256: &str,
    ) -> String {
        // Construct a kind 1063 event JSON. We use skip_validation in tests
        // so the id/sig don't need to be valid.
        format!(
            r#"["EVENT", "test_sub", {{
                "id": "0000000000000000000000000000000000000000000000000000000000000001",
                "pubkey": "{pubkey}",
                "created_at": 1700000000,
                "kind": 1063,
                "tags": [
                    ["url", "{url}"],
                    ["x", "{sha256}"],
                    ["version", "{version}"],
                    ["name", "{asset_name}"],
                    ["m", "application/gzip"],
                    ["size", "12345678"]
                ],
                "content": "",
                "sig": "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
            }}]"#
        )
    }

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
    fn test_release_filter_builds() {
        let filters = release_filter();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn test_release_pubkey_parses() {
        let pk = release_pubkey();
        assert_ne!(pk, [0u8; 32]);
    }

    #[tokio::test]
    async fn test_parse_release_note_matching_platform() {
        let (_tmp, ndb) = test_ndb();
        let asset_name = target_asset_name();
        let expected_sha = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let ev = make_release_event_json(
            RELEASE_PUBKEY_HEX,
            "99.0.0", // far future version so it's always "newer"
            asset_name,
            "https://example.com/download/test.tar.gz",
            expected_sha,
        );

        // Use a broad filter to make sure the event gets ingested
        let filter = Filter::new().kinds([RELEASE_KIND]).limit(10).build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        ndb.process_event_with(&ev, IngestMetadata::new()).unwrap();

        let nks = ndb.wait_for_notes(sub, 1).await.unwrap();
        assert_eq!(nks.len(), 1);

        let txn = Transaction::new(&ndb).unwrap();
        let note = ndb.get_note_by_key(&txn, nks[0]).unwrap();

        let release = parse_release_note(&note).expect("should parse release note");
        assert_eq!(release.version, "99.0.0");
        assert_eq!(release.asset_name, asset_name);
        assert_eq!(release.expected_sha256, expected_sha);
        assert_eq!(
            release.asset_url,
            "https://example.com/download/test.tar.gz"
        );
    }

    #[tokio::test]
    async fn test_parse_release_note_wrong_platform() {
        let (_tmp, ndb) = test_ndb();

        // Use a platform name that doesn't match the current one
        let wrong_platform = if target_asset_name().contains("linux") {
            "notedeck-x86_64-windows.zip"
        } else {
            "notedeck-x86_64-linux.tar.gz"
        };

        let ev = make_release_event_json(
            RELEASE_PUBKEY_HEX,
            "99.0.0",
            wrong_platform,
            "https://example.com/download/wrong.tar.gz",
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        );

        let filter = Filter::new().kinds([RELEASE_KIND]).limit(10).build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        ndb.process_event_with(&ev, IngestMetadata::new()).unwrap();

        let nks = ndb.wait_for_notes(sub, 1).await.unwrap();
        let txn = Transaction::new(&ndb).unwrap();
        let note = ndb.get_note_by_key(&txn, nks[0]).unwrap();

        assert!(
            matches!(
                parse_release_note(&note),
                Err(ReleaseParseError::WrongPlatform { .. })
            ),
            "should not match wrong platform: {:?}",
            parse_release_note(&note)
        );
    }

    #[tokio::test]
    async fn test_parse_release_note_older_version() {
        let (_tmp, ndb) = test_ndb();
        let asset_name = target_asset_name();

        // Use version 0.0.1 which should be older than any current version
        let ev = make_release_event_json(
            RELEASE_PUBKEY_HEX,
            "0.0.1",
            asset_name,
            "https://example.com/download/old.tar.gz",
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        );

        let filter = Filter::new().kinds([RELEASE_KIND]).limit(10).build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        ndb.process_event_with(&ev, IngestMetadata::new()).unwrap();

        let nks = ndb.wait_for_notes(sub, 1).await.unwrap();
        let txn = Transaction::new(&ndb).unwrap();
        let note = ndb.get_note_by_key(&txn, nks[0]).unwrap();

        assert!(
            matches!(
                parse_release_note(&note),
                Err(ReleaseParseError::NotNewer { .. })
            ),
            "should not return older version: {:?}",
            parse_release_note(&note)
        );
    }

    #[tokio::test]
    async fn test_find_latest_release() {
        let (_tmp, ndb) = test_ndb();
        let asset_name = target_asset_name();

        // Ingest two release events with different versions
        let ev1 = make_release_event_json(
            RELEASE_PUBKEY_HEX,
            "98.0.0",
            asset_name,
            "https://example.com/download/v98.tar.gz",
            "aaaa000000000000000000000000000000000000000000000000000000000000",
        );
        // Need a different event id for the second one
        let ev2 = make_release_event_json(
            RELEASE_PUBKEY_HEX,
            "99.0.0",
            asset_name,
            "https://example.com/download/v99.tar.gz",
            "bbbb000000000000000000000000000000000000000000000000000000000000",
        )
        .replace(
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000002",
        );

        let filter = Filter::new().kinds([RELEASE_KIND]).limit(10).build();
        let sub = ndb.subscribe(&[filter]).unwrap();

        ndb.process_event_with(&ev1, IngestMetadata::new()).unwrap();
        ndb.process_event_with(&ev2, IngestMetadata::new()).unwrap();

        // Wait for both events to be ingested
        let _ = ndb.wait_for_notes(sub, 2).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let release = find_latest_release(&ndb, &txn).expect("should find a release");

        assert_eq!(release.version, "99.0.0");
        assert_eq!(
            release.expected_sha256,
            "bbbb000000000000000000000000000000000000000000000000000000000000"
        );
    }
}
