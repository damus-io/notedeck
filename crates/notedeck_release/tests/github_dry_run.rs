use std::process::Command;

/// Fetch the latest release version that has artifacts from the GitHub API.
/// `/releases/latest` only returns non-prerelease, so we list all releases
/// and pick the first one that has assets.
fn latest_github_version_with_assets() -> String {
    let response = ureq::get("https://api.github.com/repos/damus-io/notedeck/releases?per_page=10")
        .set("User-Agent", "notedeck-release-test")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .expect("GitHub API call failed");

    let releases: Vec<serde_json::Value> = response.into_json().expect("parse JSON");

    for release in &releases {
        let assets = release["assets"].as_array();
        if let Some(assets) = assets {
            if !assets.is_empty() {
                let tag = release["tag_name"].as_str().expect("no tag_name");
                return tag.strip_prefix('v').unwrap_or(tag).to_string();
            }
        }
    }

    panic!("no GitHub release found with assets");
}

/// A throwaway secret key for dry-run signing (never used on a relay)
const TEST_SECRET_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";

#[test]
#[ignore] // requires a GitHub release with artifacts; run with: cargo test -p notedeck_release -- --ignored
fn test_github_dry_run() {
    let version = latest_github_version_with_assets();
    eprintln!("testing dry-run against latest release: v{version}");

    let bin = env!("CARGO_BIN_EXE_notedeck-release");
    let output = Command::new(bin)
        .args([
            "--version",
            &version,
            "--nsec",
            TEST_SECRET_HEX,
            "--dry-run",
        ])
        .output()
        .expect("failed to run notedeck-release");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stderr:\n{stderr}");

    assert!(
        output.status.success(),
        "notedeck-release exited with failure:\nstderr: {stderr}"
    );

    // Should have printed at least one JSON event to stdout
    assert!(!stdout.is_empty(), "no events printed to stdout");

    // NIP-82 output: N asset events (kind 3063) + 1 release event (kind 30063)
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line should be valid JSON"))
        .collect();

    let asset_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e["kind"] == 3063).collect();
    let release_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e["kind"] == 30063).collect();

    assert!(
        !asset_events.is_empty(),
        "expected at least 1 asset event (kind 3063)"
    );
    assert_eq!(
        release_events.len(),
        1,
        "expected exactly 1 release event (kind 30063)"
    );

    // Validate asset events (kind 3063)
    for event in &asset_events {
        assert!(event["sig"].is_string(), "event should be signed");
        assert!(event["pubkey"].is_string(), "event should have pubkey");

        let tags = event["tags"].as_array().expect("tags should be an array");
        let tag_names: Vec<&str> = tags
            .iter()
            .filter_map(|t| t.get(0).and_then(|v| v.as_str()))
            .collect();

        assert!(tag_names.contains(&"i"), "missing i (app id) tag");
        assert!(tag_names.contains(&"url"), "missing url tag");
        assert!(tag_names.contains(&"x"), "missing x (sha256) tag");
        assert!(tag_names.contains(&"version"), "missing version tag");
        assert!(tag_names.contains(&"f"), "missing f (platform) tag");
        assert!(tag_names.contains(&"m"), "missing m (mime) tag");
        assert!(tag_names.contains(&"size"), "missing size tag");

        // Verify app id
        let i_tag = tags
            .iter()
            .find(|t| t.get(0).and_then(|v| v.as_str()) == Some("i"))
            .expect("i tag");
        assert_eq!(
            i_tag[1].as_str().unwrap(),
            "com.damus.notedeck",
            "app id mismatch"
        );

        // Verify version tag matches
        let version_tag = tags
            .iter()
            .find(|t| t.get(0).and_then(|v| v.as_str()) == Some("version"))
            .expect("version tag");
        assert_eq!(
            version_tag[1].as_str().unwrap(),
            version,
            "version tag mismatch"
        );

        // Verify x tag is a valid 64-char hex sha256
        let x_tag = tags
            .iter()
            .find(|t| t.get(0).and_then(|v| v.as_str()) == Some("x"))
            .expect("x tag");
        let hash = x_tag[1].as_str().unwrap();
        assert_eq!(hash.len(), 64, "sha256 should be 64 hex chars");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 should be hex"
        );

        // Verify URL points to GitHub
        let url_tag = tags
            .iter()
            .find(|t| t.get(0).and_then(|v| v.as_str()) == Some("url"))
            .expect("url tag");
        let url = url_tag[1].as_str().unwrap();
        assert!(
            url.starts_with("https://github.com/damus-io/notedeck/releases/download/"),
            "url should point to GitHub releases: {url}"
        );
    }

    // Validate release event (kind 30063)
    let release = release_events[0];
    let tags = release["tags"].as_array().expect("tags should be an array");
    let tag_names: Vec<&str> = tags
        .iter()
        .filter_map(|t| t.get(0).and_then(|v| v.as_str()))
        .collect();

    assert!(tag_names.contains(&"d"), "missing d (addressable id) tag");
    assert!(tag_names.contains(&"i"), "missing i (app id) tag");
    assert!(tag_names.contains(&"version"), "missing version tag");
    assert!(tag_names.contains(&"c"), "missing c (channel) tag");
    assert!(tag_names.contains(&"e"), "missing e (asset reference) tag");

    // Verify channel defaults to "main"
    let c_tag = tags
        .iter()
        .find(|t| t.get(0).and_then(|v| v.as_str()) == Some("c"))
        .expect("c tag");
    assert_eq!(
        c_tag[1].as_str().unwrap(),
        "main",
        "default channel should be main"
    );

    // Verify the release references at least one asset
    let e_tags: Vec<&serde_json::Value> = tags
        .iter()
        .filter(|t| t.get(0).and_then(|v| v.as_str()) == Some("e"))
        .collect();
    assert_eq!(
        e_tags.len(),
        asset_events.len(),
        "release should reference all asset events"
    );

    eprintln!(
        "validated {} asset events + 1 release event",
        asset_events.len()
    );
}
