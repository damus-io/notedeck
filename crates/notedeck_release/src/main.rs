use nostrdb::NoteBuilder;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use tungstenite::{connect, Message};

fn usage() {
    eprintln!(
        "Usage: notedeck-release --version <semver> --nsec <nsec_or_hex> --relay <wss://...> [--relay ...] [options]"
    );
    eprintln!();
    eprintln!("Publishes NIP-94 file metadata events (kind 1063) for each release artifact.");
    eprintln!("By default, fetches artifacts from the GitHub Release for the given version.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --version    Release version (semver, e.g. 1.2.0)");
    eprintln!("  --nsec       Secret key (nsec bech32 or hex)");
    eprintln!("  --relay      Relay URL (repeatable)");
    eprintln!("  --dry-run    Print events as JSON without publishing");
    eprintln!("  <files...>   Local artifact files (skips GitHub fetch)");
    std::process::exit(1);
}

const GITHUB_REPO: &str = "damus-io/notedeck";

const RELEASE_ARTIFACTS: &[&str] = &[
    "notedeck-x86_64-linux.tar.gz",
    "notedeck-aarch64-linux.tar.gz",
    "notedeck-x86_64-macos.tar.gz",
    "notedeck-aarch64-macos.tar.gz",
    "notedeck-x86_64-windows.zip",
    "notedeck-aarch64-windows.zip",
];

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
    } else {
        "application/octet-stream"
    }
}

fn github_download_url(version: &str, filename: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{filename}")
}

struct ArtifactInfo {
    name: String,
    url: String,
    sha256: String,
    size: usize,
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

fn fetch_github_artifacts(version: &str) -> Result<Vec<ArtifactInfo>, String> {
    let api_url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/v{version}");
    eprintln!("fetching release info from {api_url}...");

    let response = ureq::get(&api_url)
        .set("User-Agent", "notedeck-release")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("GitHub API: {e}"))?;

    let body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("parse GitHub response: {e}"))?;

    let assets = body["assets"]
        .as_array()
        .ok_or("no assets in GitHub release")?;

    let mut artifacts = Vec::new();
    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("").to_string();
        if !RELEASE_ARTIFACTS.contains(&name.as_str()) {
            eprintln!("  skipping unknown asset: {name}");
            continue;
        }
        let browser_url = asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| format!("no download URL for {name}"))?;

        eprintln!("  downloading {name}...");
        let data = http_get(browser_url)?;
        let sha256 = sha256_hex(&data);
        let size = data.len();
        let url = github_download_url(version, &name);
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
    Ok(artifacts)
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

fn build_release_event(
    seckey: &[u8; 32],
    version: &str,
    artifact: &ArtifactInfo,
) -> Result<String, String> {
    let mime = mime_for_artifact(&artifact.name);
    let size = artifact.size.to_string();

    let note = NoteBuilder::new()
        .kind(1063)
        .content("")
        .sign(seckey)
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
        .tag_str("name")
        .tag_str(&artifact.name)
        .start_tag()
        .tag_str("m")
        .tag_str(mime)
        .start_tag()
        .tag_str("size")
        .tag_str(&size)
        .build()
        .ok_or("failed to build note")?;

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

    let version = version.unwrap_or_else(|| {
        eprintln!("error: --version required");
        usage();
        unreachable!()
    });

    let nsec_str = nsec.unwrap_or_else(|| {
        eprintln!("error: --nsec required");
        usage();
        unreachable!()
    });

    if !dry_run && relays.is_empty() {
        eprintln!("error: at least one --relay required (or use --dry-run)");
        usage();
    }

    let seckey = parse_secret_key(&nsec_str).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    if semver::Version::parse(&version).is_err() {
        eprintln!("error: invalid semver version: {version}");
        std::process::exit(1);
    }

    let artifacts = if local_files.is_empty() {
        eprintln!("fetching artifacts from GitHub Release v{version}...");
        fetch_github_artifacts(&version).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        })
    } else {
        for f in &local_files {
            if !f.exists() {
                eprintln!("error: artifact not found: {}", f.display());
                std::process::exit(1);
            }
        }
        load_local_artifacts(&version, &local_files).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        })
    };

    eprintln!("building {} release events...", artifacts.len());
    for artifact in &artifacts {
        eprintln!("  {}", artifact.name);
        let event_json = build_release_event(&seckey, &version, artifact).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

        if dry_run {
            println!("{event_json}");
            continue;
        }
        for relay in &relays {
            if let Err(e) = publish_event(relay, &event_json) {
                eprintln!("    error: {e}");
            }
        }
    }
    eprintln!("done.");
}
