//! Small shared formatting helpers for rendering comment threads, used by both
//! the CLI (`headway_cli`) and the egui board (`notedeck_headway`) so the two
//! present authors and timestamps identically.

use enostr::Pubkey;

/// A short, recognisable stand-in for a comment author: the first 12 hex chars
/// of their pubkey. There's no profile lookup at this layer, so this is just a
/// stable handle.
pub fn short_author(author: &[u8; 32]) -> String {
    Pubkey::new(*author).hex().chars().take(12).collect()
}

/// A coarse "x ago" rendering of a unix timestamp for the comment thread.
pub fn rel_time(created_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(created_at);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        86400..=604_799 => format!("{}d ago", secs / 86400),
        _ => format!("{}w ago", secs / 604_800),
    }
}
