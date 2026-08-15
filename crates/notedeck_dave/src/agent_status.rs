/// Represents the current status of an agent in the RTS scene
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentStatus {
    /// Agent is idle, no active work
    #[default]
    Idle,
    /// Agent is actively processing (receiving tokens, executing tools)
    Working,
    /// Agent needs user input (permission request pending)
    NeedsInput,
    /// Agent encountered an error
    Error,
    /// Agent completed its task successfully
    Done,
    /// Session is a placeholder waiting for the remote host to respond
    Pending,
}

impl AgentStatus {
    /// Get the color associated with this status
    pub fn color(&self) -> egui::Color32 {
        match self {
            AgentStatus::Idle => egui::Color32::from_rgb(128, 128, 128), // Gray
            AgentStatus::Working => egui::Color32::from_rgb(50, 205, 50), // Green
            AgentStatus::NeedsInput => egui::Color32::from_rgb(255, 200, 0), // Yellow/amber
            AgentStatus::Error => egui::Color32::from_rgb(220, 60, 60),  // Red
            AgentStatus::Done => egui::Color32::from_rgb(70, 130, 220),  // Blue
            AgentStatus::Pending => egui::Color32::from_rgb(100, 160, 180), // Muted cyan
        }
    }

    /// Get a human-readable label for this status
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "Idle",
            AgentStatus::Working => "Working",
            AgentStatus::NeedsInput => "Needs Input",
            AgentStatus::Error => "Error",
            AgentStatus::Done => "Done",
            AgentStatus::Pending => "Pending",
        }
    }

    /// Get the status as a lowercase string for serialization (nostr events).
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::NeedsInput => "needs_input",
            AgentStatus::Error => "error",
            AgentStatus::Done => "done",
            AgentStatus::Pending => "pending",
        }
    }

    /// Parse a status string from a nostr event (kind-31988 content).
    pub fn from_status_str(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(AgentStatus::Idle),
            "working" => Some(AgentStatus::Working),
            "needs_input" => Some(AgentStatus::NeedsInput),
            "error" => Some(AgentStatus::Error),
            "done" => Some(AgentStatus::Done),
            "pending" => Some(AgentStatus::Pending),
            _ => None,
        }
    }
}

/// Seconds of inactivity before a status color starts to fade.
const FRESH_SECS: u64 = 5 * 60;
/// Seconds of inactivity at which a status color is fully faded.
const STALE_SECS: u64 = 60 * 60;
/// Saturation multiplier applied at full staleness. Keeps the hue recognisable
/// (a washed-out version of the color) rather than fading all the way to gray.
const STALE_SATURATION: f32 = 0.4;

/// How stale a session is, from `0.0` (fresh) to `1.0` (fully stale), given
/// `age_secs` seconds since its last activity. `0.0` under [`FRESH_SECS`], then a
/// linear ramp to `1.0` at [`STALE_SECS`].
fn staleness(age_secs: u64) -> f32 {
    if age_secs <= FRESH_SECS {
        return 0.0;
    }
    let span = (STALE_SECS - FRESH_SECS) as f32;
    (((age_secs - FRESH_SECS) as f32) / span).clamp(0.0, 1.0)
}

/// Fade a status `color` toward a desaturated version of itself (same hue) as a
/// session goes stale. `age_secs` is the seconds since the session last did
/// anything: the color is returned unchanged while fresh (under [`FRESH_SECS`]),
/// then its saturation ramps down to [`STALE_SATURATION`] of the original by
/// [`STALE_SECS`]. Gives the user an at-a-glance "this has gone quiet" signal
/// without losing the status hue.
///
/// Pure and allocation-free — safe to call every frame in the render path.
pub fn stale_color(color: egui::Color32, age_secs: u64) -> egui::Color32 {
    let t = staleness(age_secs);
    if t <= 0.0 {
        // Fresh: return the color untouched (no lossy Hsva round-trip).
        return color;
    }
    let mut hsva = egui::ecolor::Hsva::from(color);
    hsva.s *= 1.0 + (STALE_SATURATION - 1.0) * t;
    egui::Color32::from(hsva)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh session's color is returned byte-identical (no fade, no round-trip
    /// loss), both below the fresh window and exactly at its edge.
    #[test]
    fn fresh_color_is_unchanged() {
        let green = AgentStatus::Working.color();
        assert_eq!(stale_color(green, 0), green);
        assert_eq!(stale_color(green, FRESH_SECS), green);
    }

    /// A fully stale color keeps its hue but drops to [`STALE_SATURATION`] of its
    /// original saturation.
    #[test]
    fn fully_stale_desaturates_but_keeps_hue() {
        let green = AgentStatus::Working.color();
        let faded = stale_color(green, STALE_SECS);
        assert_ne!(faded, green);

        let before = egui::ecolor::Hsva::from(green);
        let after = egui::ecolor::Hsva::from(faded);
        // Hue preserved (allow small 8-bit round-trip error).
        assert!((after.h - before.h).abs() < 0.02, "hue drifted");
        // Saturation cut to STALE_SATURATION of the original.
        assert!(
            (after.s - before.s * STALE_SATURATION).abs() < 0.05,
            "expected ~{} saturation, got {}",
            before.s * STALE_SATURATION,
            after.s
        );
    }

    /// A mid-range age fades partway: less saturated than fresh, more than fully
    /// stale.
    #[test]
    fn mid_range_is_between() {
        let green = AgentStatus::Working.color();
        let mid_age = (FRESH_SECS + STALE_SECS) / 2;
        let mid = egui::ecolor::Hsva::from(stale_color(green, mid_age)).s;
        let fresh = egui::ecolor::Hsva::from(green).s;
        let stale = egui::ecolor::Hsva::from(stale_color(green, STALE_SECS)).s;
        assert!(stale < mid && mid < fresh, "{stale} < {mid} < {fresh}");
    }
}
