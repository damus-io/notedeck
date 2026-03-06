use tracing::{info, warn};

/// Information about a release asset available for download
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub asset_url: String,
    pub asset_name: String,
}

/// Returns the expected asset name for the current platform/arch
fn target_asset_name() -> &'static str {
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

const GITHUB_API_URL: &str = "https://api.github.com/repos/damus-io/notedeck/releases/latest";

/// Check GitHub Releases for a newer version. Calls the callback with
/// `Ok(Some(ReleaseInfo))` if an update is available, `Ok(None)` if
/// up-to-date, or `Err` on failure.
pub fn check_for_update(
    current_version: &str,
    on_done: impl FnOnce(Result<Option<ReleaseInfo>, String>) + Send + 'static,
) {
    let current_version = current_version.to_string();

    let mut request = ehttp::Request::get(GITHUB_API_URL);
    request
        .headers
        .insert("User-Agent".to_string(), "notedeck-updater".to_string());
    request.headers.insert(
        "Accept".to_string(),
        "application/vnd.github+json".to_string(),
    );

    ehttp::fetch(request, move |response| {
        let result = parse_update_response(&current_version, response);
        on_done(result);
    });
}

fn parse_update_response(
    current_version: &str,
    response: Result<ehttp::Response, String>,
) -> Result<Option<ReleaseInfo>, String> {
    let response = response.map_err(|e| format!("HTTP request failed: {e}"))?;

    if response.status != 200 {
        return Err(format!("GitHub API returned status {}", response.status));
    }

    let body = response
        .text()
        .ok_or_else(|| "Response body is not valid UTF-8".to_string())?;

    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let tag_name = json["tag_name"]
        .as_str()
        .ok_or_else(|| "Missing tag_name in release".to_string())?;

    let remote_version_str = tag_name.strip_prefix('v').unwrap_or(tag_name);

    let current = semver::Version::parse(current_version)
        .map_err(|e| format!("Failed to parse current version '{current_version}': {e}"))?;
    let remote = semver::Version::parse(remote_version_str)
        .map_err(|e| format!("Failed to parse remote version '{remote_version_str}': {e}"))?;

    if remote <= current {
        info!("up to date: current={current_version}, latest={remote_version_str}");
        return Ok(None);
    }

    info!("update available: {current_version} -> {remote_version_str}");

    let expected_name = target_asset_name();
    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| "Missing assets array in release".to_string())?;

    for asset in assets {
        let name = asset["name"].as_str().unwrap_or_default();
        if name == expected_name {
            let download_url = asset["browser_download_url"]
                .as_str()
                .ok_or_else(|| "Missing download URL for asset".to_string())?;

            return Ok(Some(ReleaseInfo {
                version: remote_version_str.to_string(),
                asset_url: download_url.to_string(),
                asset_name: name.to_string(),
            }));
        }
    }

    warn!("update {remote_version_str} found but no matching asset '{expected_name}'");
    Err(format!(
        "No matching asset '{expected_name}' in release {remote_version_str}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_response(json: &str) -> Result<ehttp::Response, String> {
        Ok(ehttp::Response {
            status: 200,
            status_text: "OK".to_string(),
            url: GITHUB_API_URL.to_string(),
            bytes: json.as_bytes().to_vec(),
            headers: Default::default(),
            ok: true,
        })
    }

    #[test]
    fn test_up_to_date() {
        let json = r#"{"tag_name": "v0.7.1", "assets": []}"#;
        let result = parse_update_response("0.7.1", make_response(json));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_older_remote() {
        let json = r#"{"tag_name": "v0.6.0", "assets": []}"#;
        let result = parse_update_response("0.7.1", make_response(json));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_update_available() {
        let expected = target_asset_name();
        let json = format!(
            r#"{{
                "tag_name": "v1.0.0",
                "assets": [
                    {{
                        "name": "{expected}",
                        "browser_download_url": "https://example.com/download/{expected}"
                    }}
                ]
            }}"#
        );
        let result = parse_update_response("0.7.1", make_response(&json));
        let info = result.unwrap().unwrap();
        assert_eq!(info.version, "1.0.0");
        assert_eq!(info.asset_name, expected);
    }

    #[test]
    fn test_update_no_matching_asset() {
        let json = r#"{
            "tag_name": "v1.0.0",
            "assets": [
                {"name": "some-other-asset.zip", "browser_download_url": "https://example.com/other"}
            ]
        }"#;
        let result = parse_update_response("0.7.1", make_response(json));
        assert!(result.is_err());
    }

    #[test]
    fn test_http_error() {
        let result = parse_update_response("0.7.1", Err("connection refused".to_string()));
        assert!(result.is_err());
    }
}
