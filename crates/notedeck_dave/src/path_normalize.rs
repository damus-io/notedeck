//! Path normalization for JSONL source-data.
//!
//! When storing JSONL lines in nostr events, absolute paths are converted to
//! relative (using the session's `cwd` as base). On reconstruction, relative
//! paths are re-expanded using the local machine's working directory.
//!
//! This operates on the raw JSON string via string replacement — paths can
//! appear anywhere in tool inputs/outputs, so structural replacement would
//! miss nested occurrences.

/// Replace all occurrences of `cwd` prefix in absolute paths with relative paths.
///
/// Not currently used (Phase 1 stores raw paths), kept for future Phase 2.
#[allow(dead_code)]
///
/// For example, with cwd = "/Users/jb55/dev/notedeck":
///   "/Users/jb55/dev/notedeck/src/main.rs" → "src/main.rs"
///   "/Users/jb55/dev/notedeck" → "."
pub fn normalize_paths(json: &str, cwd: &str) -> String {
    if cwd.is_empty() {
        return json.to_string();
    }

    // Ensure cwd doesn't have a trailing slash for consistent matching
    let cwd = cwd.strip_suffix('/').unwrap_or(cwd);

    // Replace "cwd/" prefix first (subpaths), then bare "cwd" (exact match)
    let with_slash = format!("{}/", cwd);
    let result = json.replace(&with_slash, "");

    // Replace bare cwd (e.g. the cwd field itself) with "."
    result.replace(cwd, ".")
}

/// Re-expand relative paths back to absolute using the given local cwd.
///
/// Reverses `normalize_paths`: the cwd field "." becomes the local cwd,
/// and relative paths get the cwd prefix prepended.
///
/// Note: This is not perfectly inverse — it will also expand any unrelated
/// "." occurrences that happen to match. In practice, the cwd field is the
/// main target, and relative paths in tool inputs/outputs are the rest.
///
/// Not currently used (Phase 1 stores raw paths), kept for future Phase 2.
#[allow(dead_code)]
pub fn denormalize_paths(json: &str, local_cwd: &str) -> String {
    if local_cwd.is_empty() {
        return json.to_string();
    }

    let local_cwd = local_cwd.strip_suffix('/').unwrap_or(local_cwd);

    // We need to be careful about ordering here. We want to:
    // 1. Replace "." (bare cwd reference) with the local cwd
    // 2. Re-expand relative paths that were stripped of the cwd prefix
    //
    // But since normalized JSON has paths like "src/main.rs" (no prefix),
    // we can't blindly prefix all bare paths. Instead, we reverse the
    // exact transformations that normalize_paths applied:
    //
    // The normalize step replaced:
    //   "{cwd}/" → ""  (paths become relative)
    //   "{cwd}"  → "." (bare cwd references)
    //
    // So to reverse, we need context-aware replacement. The safest approach
    // is to look for patterns that were likely produced by normalization:
    //   - JSON string values that are exactly "." → local_cwd
    //   - Relative paths in known field positions
    //
    // For now, we do simple string replacement which handles the most
    // common case (the "cwd" field). Full path reconstruction for tool
    // inputs/outputs would need the original field structure.

    // Replace "\"cwd\":\".\"" with the local cwd
    let result = json.replace("\"cwd\":\".\"", &format!("\"cwd\":\"{}\"", local_cwd));

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_absolute_paths() {
        let json =
            r#"{"cwd":"/Users/jb55/dev/notedeck","file":"/Users/jb55/dev/notedeck/src/main.rs"}"#;
        let normalized = normalize_paths(json, "/Users/jb55/dev/notedeck");
        assert_eq!(normalized, r#"{"cwd":".","file":"src/main.rs"}"#);
    }

    #[test]
    fn test_normalize_with_trailing_slash() {
        // cwd with trailing slash is stripped; the cwd value in JSON
        // still contains the trailing slash so it becomes "" + "/" = "/"
        // after replacing the base. In practice JSONL cwd values don't
        // have trailing slashes.
        let json = r#"{"cwd":"/tmp/project","file":"/tmp/project/lib.rs"}"#;
        let normalized = normalize_paths(json, "/tmp/project/");
        assert_eq!(normalized, r#"{"cwd":".","file":"lib.rs"}"#);
    }

    #[test]
    fn test_normalize_empty_cwd() {
        let json = r#"{"file":"/some/path"}"#;
        let normalized = normalize_paths(json, "");
        assert_eq!(normalized, json);
    }

    #[test]
    fn test_normalize_no_matching_paths() {
        let json = r#"{"file":"/other/path/file.rs"}"#;
        let normalized = normalize_paths(json, "/Users/jb55/dev/notedeck");
        assert_eq!(normalized, json);
    }

    #[test]
    fn test_normalize_multiple_occurrences() {
        let json =
            r#"{"old":"/Users/jb55/dev/notedeck/a.rs","new":"/Users/jb55/dev/notedeck/b.rs"}"#;
        let normalized = normalize_paths(json, "/Users/jb55/dev/notedeck");
        assert_eq!(normalized, r#"{"old":"a.rs","new":"b.rs"}"#);
    }

    #[test]
    fn test_denormalize_cwd_field() {
        let json = r#"{"cwd":"."}"#;
        let denormalized = denormalize_paths(json, "/Users/jb55/dev/notedeck");
        assert_eq!(denormalized, r#"{"cwd":"/Users/jb55/dev/notedeck"}"#);
    }

    #[test]
    fn test_normalize_roundtrip_cwd() {
        let original_cwd = "/Users/jb55/dev/notedeck";
        let json = r#"{"cwd":"/Users/jb55/dev/notedeck"}"#;
        let normalized = normalize_paths(json, original_cwd);
        assert_eq!(normalized, r#"{"cwd":"."}"#);
        let denormalized = denormalize_paths(&normalized, original_cwd);
        assert_eq!(denormalized, json);
    }
}
