//! The permission-mode vocabulary shared across the session wire format.
//!
//! A session's permission mode decides how its agent's tool calls are gated. It
//! travels on three different events — the `permission-mode` tag on kind-31988
//! state, the payload of a kind-1988 `set_permission_mode` command, and the
//! `permission_mode` tag on a kind-31989 spawn command — always as one of the
//! canonical strings in [`PERMISSION_MODES`].
//!
//! The strings themselves are dictated by the host: `notedeck_dave`'s
//! `permission_mode_to_str` / `permission_mode_from_str` map them onto the
//! backend SDK's `PermissionMode`. They live here, in the crate that owns the
//! wire format, because the *clients* that set a mode (the `agentium` CLI, and
//! eventually the iOS app) can't depend on the host crate to learn what is
//! valid.

/// Every permission mode the wire format speaks.
///
/// Ordered as dave's UI cycles them — Manual, Plan, Accept Edits, Auto — with
/// `bypass` last, since it is deliberately outside that cycle (it does no safety
/// checking at all, so it can only be entered deliberately).
pub const PERMISSION_MODES: [&str; 5] = ["default", "plan", "accept_edits", "auto", "bypass"];

/// Normalize a caller-supplied permission mode to its canonical wire spelling,
/// or `None` if it names no known mode.
///
/// Accepts the canonical strings plus the spellings a human is likely to reach
/// for: the labels dave's own badge shows (`manual` for `default`), and Claude
/// Code's camelCase names (`acceptEdits`, `bypassPermissions`). Comparison
/// ignores surrounding whitespace, ASCII case, and `-` vs `_`, so `Accept-Edits`
/// and `acceptEdits` both land on `accept_edits`.
///
/// Returning the canonical `&'static str` rather than a bool means a caller that
/// validates also gets the string to put on the wire, so an alias can never leak
/// into an event.
pub fn parse_permission_mode(mode: &str) -> Option<&'static str> {
    let normalized = mode.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        // "MANUAL" is what dave's badge calls Default.
        "default" | "manual" => Some("default"),
        "plan" => Some("plan"),
        "accept_edits" | "acceptedits" => Some("accept_edits"),
        "auto" => Some("auto"),
        "bypass" | "bypass_permissions" | "bypasspermissions" => Some("bypass"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical mode parses back to itself, so the constant and the
    /// parser can't drift apart.
    #[test]
    fn canonical_modes_round_trip() {
        for mode in PERMISSION_MODES {
            assert_eq!(parse_permission_mode(mode), Some(mode));
        }
    }

    #[test]
    fn aliases_normalize_to_canonical_spellings() {
        assert_eq!(parse_permission_mode("manual"), Some("default"));
        assert_eq!(parse_permission_mode("acceptEdits"), Some("accept_edits"));
        assert_eq!(parse_permission_mode("accept-edits"), Some("accept_edits"));
        assert_eq!(parse_permission_mode("  PLAN "), Some("plan"));
        assert_eq!(parse_permission_mode("bypassPermissions"), Some("bypass"));
    }

    #[test]
    fn unknown_modes_are_rejected() {
        // Not silently mapped to a default: a caller that can't tell an unknown
        // mode from a valid one would spawn sessions in the wrong mode.
        assert_eq!(parse_permission_mode("yolo"), None);
        assert_eq!(parse_permission_mode(""), None);
        // A near-miss on a real mode is still a miss.
        assert_eq!(parse_permission_mode("planning"), None);
    }
}
