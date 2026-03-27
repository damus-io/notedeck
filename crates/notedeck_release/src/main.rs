use nostrdb::NoteBuilder;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use tungstenite::{connect, Message};

/// NIP-82 app identifier
const APP_ID: &str = "io.damus.notedeck";

fn usage() {
    eprintln!(
        "Usage: notedeck-release --nsec <nsec_or_hex> --relay <wss://...> [--relay ...] [options]"
    );
    eprintln!();
    eprintln!(
        "Publishes NIP-82 release events (kind 30063 + kind 3063) for each release artifact."
    );
    eprintln!("By default, fetches artifacts from the latest GitHub Release (or a specific version with --version).");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --version    Release version (semver, e.g. 1.2.0; defaults to latest)");
    eprintln!("  --nsec       Secret key (nsec bech32 or hex)");
    eprintln!("  --relay      Relay URL (repeatable)");
    eprintln!("  --channel    Release channel: main (default), beta, nightly, dev");
    eprintln!("  --dry-run    Print events as JSON without publishing");
    eprintln!("  <files...>   Local artifact files (skips GitHub fetch)");
    std::process::exit(1);
}

const GITHUB_REPO: &str = "damus-io/notedeck";

/// File extensions recognized as release artifacts
const ARTIFACT_EXTENSIONS: &[&str] = &[
    ".tar.gz", ".zip", ".dmg", ".deb", ".rpm", ".exe", ".msi", ".apk",
];

/// Valid release channels
const VALID_CHANNELS: &[&str] = &["main", "beta", "nightly", "dev"];

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn mime_for_artifact(name: &str) -> &'static str {
    if name.ends_with(".tar.gz") {
        "application/gzip"
    } else if name.ends_with(".zip") {
        "application/zip"
    } else if name.ends_with(".dmg") {
        "application/x-apple-diskimage"
    } else if name.ends_with(".apk") {
        "application/vnd.android.package-archive"
    } else {
        "application/octet-stream"
    }
}

fn github_download_url(version: &str, filename: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{filename}")
}

/// Normalize architecture names to our canonical form.
/// amd64/x86_64 → x86_64, arm64/aarch64 → aarch64
fn normalize_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

/// Derive the NIP-82 platform "f" tag from an artifact filename.
///
/// Handles all notedeck artifact naming patterns:
///   notedeck-x86_64-linux.tar.gz    → linux-x86_64
///   notedeck-aarch64-macos.tar.gz   → macos-aarch64
///   notedeck-aarch64.dmg            → macos-aarch64   (dmg implies macos)
///   DamusNotedeckInstaller.exe      → windows-x86_64  (exe/msi implies windows)
///   notedeck-0.8.0-1.x86_64.rpm    → linux-x86_64    (rpm implies linux)
///   notedeck_0.8.0-1_amd64.deb     → linux-x86_64    (deb implies linux, amd64=x86_64)
fn platform_tag_from_name(name: &str) -> Option<String> {
    // .apk → android (currently only aarch64 builds)
    if name.ends_with(".apk") {
        return Some("android-aarch64".to_string());
    }

    // .exe / .msi → windows (currently only x86_64 builds)
    if name.ends_with(".exe") || name.ends_with(".msi") {
        return Some("windows-x86_64".to_string());
    }

    // .dmg → macos, arch from filename: notedeck-{arch}.dmg
    if name.ends_with(".dmg") {
        let stem = name.strip_prefix("notedeck-")?.strip_suffix(".dmg")?;
        let arch = normalize_arch(stem)?;
        return Some(format!("macos-{arch}"));
    }

    // .deb → linux: notedeck_0.8.0-1_{debarch}.deb
    if name.ends_with(".deb") {
        let stem = name.strip_suffix(".deb")?;
        let deb_arch = stem.rsplit('_').next()?;
        let arch = normalize_arch(deb_arch)?;
        return Some(format!("linux-{arch}"));
    }

    // .rpm → linux: notedeck-0.8.0-1.{rpmarch}.rpm
    if name.ends_with(".rpm") {
        let stem = name.strip_suffix(".rpm")?;
        let rpm_arch = stem.rsplit('.').next()?;
        let arch = normalize_arch(rpm_arch)?;
        return Some(format!("linux-{arch}"));
    }

    // .tar.gz / .zip → notedeck-{arch}-{os}.{ext}
    let stem = name.strip_prefix("notedeck-")?;
    let stem = stem
        .strip_suffix(".tar.gz")
        .or_else(|| stem.strip_suffix(".zip"))?;

    let parts: Vec<&str> = stem.splitn(2, '-').collect();
    if parts.len() == 2 {
        let arch = normalize_arch(parts[0])?;
        Some(format!("{}-{arch}", parts[1]))
    } else {
        None
    }
}

struct ArtifactInfo {
    name: String,
    url: String,
    sha256: String,
    size: usize,
}

fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache")
    } else {
        PathBuf::from("/tmp")
    }
    .join("notedeck-release")
}

/// Look up a cached artifact by URL. Returns (data, sha256) if cached.
fn cache_lookup(url: &str) -> Option<(Vec<u8>, String)> {
    let dir = cache_dir();
    let url_hash = sha256_hex(url.as_bytes());
    let link_path = dir.join(format!("url_{url_hash}"));
    let content_hash = std::fs::read_to_string(&link_path).ok()?;
    let content_hash = content_hash.trim().to_string();
    let data = std::fs::read(dir.join(&content_hash)).ok()?;
    // verify content integrity
    if sha256_hex(&data) != content_hash {
        return None;
    }
    Some((data, content_hash))
}

/// Store an artifact in the content-addressed cache.
fn cache_store(url: &str, data: &[u8], content_hash: &str) {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(content_hash), data);
    let url_hash = sha256_hex(url.as_bytes());
    let _ = std::fs::write(dir.join(format!("url_{url_hash}")), content_hash);
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let len: usize = response
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(len);
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {url}: {e}"))?;
    Ok(bytes)
}

/// Fetch artifacts from a GitHub release. If `version` is None, uses the latest release.
/// Returns (version, artifacts).
fn fetch_github_artifacts(version: Option<&str>) -> Result<(String, Vec<ArtifactInfo>), String> {
    let (api_url, is_list) = match version {
        Some(v) => (
            format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/v{v}"),
            false,
        ),
        None => (
            format!("https://api.github.com/repos/{GITHUB_REPO}/releases?per_page=1"),
            true,
        ),
    };
    eprintln!("fetching release info from {api_url}...");

    let response = ureq::get(&api_url)
        .set("User-Agent", "notedeck-release")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("GitHub API: {e}"))?;

    let body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("parse GitHub response: {e}"))?;

    // /releases returns an array; /releases/tags/v{x} returns an object
    let release = if is_list {
        body.as_array()
            .and_then(|a| a.first().cloned())
            .ok_or("no releases found")?
    } else {
        body
    };

    let version = match version {
        Some(v) => v.to_string(),
        None => {
            let tag = release["tag_name"]
                .as_str()
                .ok_or("no tag_name in GitHub release")?;
            let v = tag.strip_prefix('v').unwrap_or(tag).to_string();
            eprintln!("latest release: v{v}");
            v
        }
    };

    let assets = release["assets"]
        .as_array()
        .ok_or("no assets in GitHub release")?;

    let mut artifacts = Vec::new();
    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("").to_string();
        if !ARTIFACT_EXTENSIONS.iter().any(|ext| name.ends_with(ext)) {
            eprintln!("  skipping non-artifact asset: {name}");
            continue;
        }
        let browser_url = asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| format!("no download URL for {name}"))?;

        let (data, sha256) = if let Some((data, hash)) = cache_lookup(browser_url) {
            eprintln!("  {name} (cached)");
            (data, hash)
        } else {
            eprintln!("  downloading {name}...");
            let data = http_get(browser_url)?;
            let hash = sha256_hex(&data);
            cache_store(browser_url, &data, &hash);
            (data, hash)
        };
        let size = data.len();
        let url = github_download_url(&version, &name);
        eprintln!("    sha256: {sha256}");
        eprintln!("    size:   {size}");
        artifacts.push(ArtifactInfo {
            name,
            url,
            sha256,
            size,
        });
    }

    if artifacts.is_empty() {
        return Err(format!(
            "no matching release artifacts found for v{version}"
        ));
    }
    Ok((version, artifacts))
}

fn load_local_artifacts(version: &str, paths: &[PathBuf]) -> Result<Vec<ArtifactInfo>, String> {
    let mut artifacts = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("bad filename: {}", path.display()))?
            .to_string();
        let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let sha256 = sha256_hex(&data);
        let size = data.len();
        let url = github_download_url(version, &name);
        artifacts.push(ArtifactInfo {
            name,
            url,
            sha256,
            size,
        });
    }
    Ok(artifacts)
}

/// Build a NIP-82 kind 3063 (Software Asset) event.
/// Returns the event JSON and the event id (needed for the release event's "e" tags).
fn build_asset_event(
    seckey: &[u8; 32],
    version: &str,
    artifact: &ArtifactInfo,
) -> Result<(String, [u8; 32]), String> {
    let mime = mime_for_artifact(&artifact.name);
    let size = artifact.size.to_string();
    let platform = platform_tag_from_name(&artifact.name).unwrap_or_else(|| "unknown".to_string());

    let note = NoteBuilder::new()
        .kind(3063)
        .content("")
        .sign(seckey)
        .start_tag()
        .tag_str("i")
        .tag_str(APP_ID)
        .start_tag()
        .tag_str("url")
        .tag_str(&artifact.url)
        .start_tag()
        .tag_str("x")
        .tag_str(&artifact.sha256)
        .start_tag()
        .tag_str("version")
        .tag_str(version)
        .start_tag()
        .tag_str("f")
        .tag_str(&platform)
        .start_tag()
        .tag_str("name")
        .tag_str(&artifact.name)
        .start_tag()
        .tag_str("m")
        .tag_str(mime)
        .start_tag()
        .tag_str("size")
        .tag_str(&size)
        .build()
        .ok_or("failed to build asset note")?;

    let id = *note.id();
    let json = note.json().map_err(|e| format!("json: {e}"))?;
    Ok((json, id))
}

/// Build a NIP-82 kind 30063 (Software Release) event referencing the given asset event ids.
fn build_release_event(
    seckey: &[u8; 32],
    version: &str,
    channel: &str,
    asset_ids: &[[u8; 32]],
) -> Result<String, String> {
    let d_tag = format!("{APP_ID}@{version}");

    let mut builder = NoteBuilder::new()
        .kind(30063)
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

    let note = builder.build().ok_or("failed to build release note")?;
    note.json().map_err(|e| format!("json: {e}"))
}

fn publish_event(relay_url: &str, event_json: &str) -> Result<(), String> {
    let msg = format!("[\"EVENT\",{}]", event_json);

    let (mut socket, _response) =
        connect(relay_url).map_err(|e| format!("connect to {relay_url}: {e}"))?;

    socket
        .send(Message::Text(msg))
        .map_err(|e| format!("send to {relay_url}: {e}"))?;

    loop {
        let reply = socket
            .read()
            .map_err(|e| format!("read from {relay_url}: {e}"))?;

        match reply {
            Message::Text(text) => {
                if text.starts_with("[\"OK\"") {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&text).map_err(|e| format!("parse OK: {e}"))?;
                    let accepted = parsed.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
                    if accepted {
                        eprintln!("  {relay_url}: accepted");
                        break;
                    } else {
                        let reason = parsed.get(3).and_then(|v| v.as_str()).unwrap_or("unknown");
                        return Err(format!("{relay_url} rejected: {reason}"));
                    }
                } else if text.starts_with("[\"NOTICE\"") {
                    eprintln!("  {relay_url} notice: {text}");
                }
            }
            Message::Close(_) => {
                return Err(format!("{relay_url}: connection closed before OK"));
            }
            _ => {}
        }
    }

    let _ = socket.close(None);
    Ok(())
}

fn parse_secret_key(input: &str) -> Result<[u8; 32], String> {
    if input.len() == 64 {
        let mut bytes = [0u8; 32];
        if hex::decode_to_slice(input, &mut bytes).is_ok() {
            return Ok(bytes);
        }
    }
    let sk = nostr::SecretKey::parse(input).map_err(|e| format!("invalid secret key: {e}"))?;
    Ok(sk.to_secret_bytes())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut version = None;
    let mut nsec = None;
    let mut relays: Vec<String> = Vec::new();
    let mut local_files: Vec<PathBuf> = Vec::new();
    let mut dry_run = false;
    let mut channel = "main".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                i += 1;
                version = args.get(i).cloned();
            }
            "--nsec" | "--sec" => {
                i += 1;
                nsec = args.get(i).cloned();
            }
            "--relay" => {
                i += 1;
                if let Some(r) = args.get(i) {
                    relays.push(r.clone());
                }
            }
            "--channel" => {
                i += 1;
                if let Some(c) = args.get(i) {
                    channel = c.clone();
                }
            }
            "--dry-run" => {
                dry_run = true;
            }
            "--help" | "-h" => usage(),
            other => {
                if other.starts_with('-') {
                    eprintln!("unknown flag: {other}");
                    usage();
                }
                local_files.push(PathBuf::from(other));
            }
        }
        i += 1;
    }

    if !local_files.is_empty() && version.is_none() {
        eprintln!("error: --version required when using local files");
        usage();
    }

    let nsec_str = nsec.unwrap_or_else(|| {
        eprintln!("error: --nsec required");
        usage();
        unreachable!()
    });

    if !dry_run && relays.is_empty() {
        eprintln!("error: at least one --relay required (or use --dry-run)");
        usage();
    }

    if !VALID_CHANNELS.contains(&channel.as_str()) {
        eprintln!(
            "error: invalid channel '{channel}'. Valid channels: {}",
            VALID_CHANNELS.join(", ")
        );
        std::process::exit(1);
    }

    let seckey = parse_secret_key(&nsec_str).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    if let Some(ref v) = version {
        if semver::Version::parse(v).is_err() {
            eprintln!("error: invalid semver version: {v}");
            std::process::exit(1);
        }
    }

    let (version, artifacts) = if local_files.is_empty() {
        let v = version.as_deref();
        if let Some(v) = v {
            eprintln!("fetching artifacts from GitHub Release v{v}...");
        } else {
            eprintln!("fetching latest GitHub Release...");
        }
        let (version, artifacts) = fetch_github_artifacts(v).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
        (version, artifacts)
    } else {
        let version = version.unwrap(); // guaranteed by check above
        for f in &local_files {
            if !f.exists() {
                eprintln!("error: artifact not found: {}", f.display());
                std::process::exit(1);
            }
        }
        let artifacts = load_local_artifacts(&version, &local_files).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
        (version, artifacts)
    };

    // Step 1: Build and publish asset events (kind 3063), collecting their ids
    eprintln!("building {} asset events (kind 3063)...", artifacts.len());
    let mut asset_ids: Vec<[u8; 32]> = Vec::new();
    let mut asset_jsons: Vec<String> = Vec::new();

    for artifact in &artifacts {
        let platform =
            platform_tag_from_name(&artifact.name).unwrap_or_else(|| "unknown".to_string());
        eprintln!("  {} (platform: {})", artifact.name, platform);

        let (json, id) = build_asset_event(&seckey, &version, artifact).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

        if dry_run {
            println!("{json}");
        } else {
            for relay in &relays {
                if let Err(e) = publish_event(relay, &json) {
                    eprintln!("    error: {e}");
                }
            }
        }

        asset_ids.push(id);
        asset_jsons.push(json);
    }

    // Step 2: Build and publish the release event (kind 30063) referencing all assets
    eprintln!("building release event (kind 30063, channel: {channel})...");
    let release_json =
        build_release_event(&seckey, &version, &channel, &asset_ids).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

    if dry_run {
        println!("{release_json}");
    } else {
        for relay in &relays {
            if let Err(e) = publish_event(relay, &release_json) {
                eprintln!("    error: {e}");
            }
        }
    }

    eprintln!("done.");
}
