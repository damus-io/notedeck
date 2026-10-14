/// Represents the current status of an agent in the RTS scene
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    /// The foreground turn has ended, but something the session started (a
    /// background task, a Monitor, a scheduled wake-up or cron) will resume
    /// it. Parked on purpose: neither finished nor needing the user.
    Waiting,
}

impl AgentStatus {
    /// Get the color associated with this status: Linear's status palette,
    /// with rose for needs-input. Statuses that share a meaning with a Headway
    /// column use that column's color from [`notedeck::tokens`], so one meaning
    /// reads as one color in both apps: working is in-progress yellow and done
    /// (a finished turn awaiting the user) is in-review green. Needs-input is
    /// rose rather than Headway's urgent amber, which sat too close to
    /// working's yellow to tell apart on a status bar.
    pub fn color(&self) -> egui::Color32 {
        use notedeck::tokens::{STATUS_IN_PROGRESS, STATUS_IN_REVIEW};
        match self {
            AgentStatus::Idle => egui::Color32::from_rgb(0x95, 0xA2, 0xB3), // Slate gray
            AgentStatus::Working => STATUS_IN_PROGRESS,
            AgentStatus::NeedsInput => egui::Color32::from_rgb(0xF2, 0x54, 0x7A), // Rose
            AgentStatus::Error => egui::Color32::from_rgb(0xEB, 0x57, 0x57),      // Red
            AgentStatus::Done => STATUS_IN_REVIEW,
            AgentStatus::Pending => egui::Color32::from_rgb(0x5E, 0x6A, 0xD2), // Indigo
            AgentStatus::Waiting => egui::Color32::from_rgb(0xBB, 0x87, 0xFC), // Violet
        }
    }

    /// Whether a session in this status asks nothing of the user: it finished
    /// or sits idle. A chain link that handed its work on dims its stretch of
    /// the chain's status bar only when it is quiet; one that needs input or
    /// errored keeps its full color, since it still wants attention.
    pub fn is_quiet(&self) -> bool {
        matches!(self, AgentStatus::Done | AgentStatus::Idle)
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
            AgentStatus::Waiting => "Waiting",
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
            AgentStatus::Waiting => "waiting",
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
            "waiting" => Some(AgentStatus::Waiting),
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

/// Width of a session's status bar: the vertical line down a session-list
/// row's left edge, and the leading glyph of an `agentium:` chip.
pub const STATUS_BAR_WIDTH: f32 = 3.0;

/// Paint a session's status bar filling `rect` in `color` (a status color,
/// usually through [`stale_color`]). The one shape the session list and the
/// inline session chip both draw, so the two read as the same thing.
pub fn paint_status_bar(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    painter.rect_filled(rect, STATUS_BAR_WIDTH / 2.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status, for the exhaustive round-trip below.
    const ALL: [AgentStatus; 7] = [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::NeedsInput,
        AgentStatus::Error,
        AgentStatus::Done,
        AgentStatus::Pending,
        AgentStatus::Waiting,
    ];

    /// The kind-31988 state event carries a status as its `as_str` token and a
    /// remote reads it back with `from_status_str`; every status, `Waiting`
    /// included, must survive that trip, and no two may share a token.
    #[test]
    fn status_round_trips_through_its_wire_token() {
        for status in ALL {
            assert_eq!(
                AgentStatus::from_status_str(status.as_str()),
                Some(status),
                "{status:?} did not round-trip"
            );
        }
        assert_eq!(AgentStatus::Waiting.as_str(), "waiting");
        let mut tokens: Vec<_> = ALL.iter().map(AgentStatus::as_str).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), ALL.len(), "two statuses share a wire token");
    }

    /// Waiting is told apart from Done and Idle at a glance.
    #[test]
    fn waiting_has_its_own_color_and_label() {
        for other in [AgentStatus::Done, AgentStatus::Idle] {
            assert_ne!(AgentStatus::Waiting.color(), other.color());
            assert_ne!(AgentStatus::Waiting.label(), other.label());
        }
    }

    /// Every status paints its own color. Working took Headway's in-progress
    /// yellow, so needs-input must not sit on a look-alike yellow beside it.
    #[test]
    fn every_status_has_a_distinct_color() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.color(), b.color(), "{a:?} and {b:?} share a color");
            }
        }
    }

    /// Statuses that mean the same thing as a Headway column paint that
    /// column's color.
    #[test]
    fn shared_meanings_use_headway_status_colors() {
        use notedeck::tokens::{STATUS_IN_PROGRESS, STATUS_IN_REVIEW};
        assert_eq!(AgentStatus::Working.color(), STATUS_IN_PROGRESS);
        assert_eq!(AgentStatus::Done.color(), STATUS_IN_REVIEW);
    }

    /// A fresh session's color is returned byte-identical (no fade, no round-trip
    /// loss), both below the fresh window and exactly at its edge.
    #[test]
    fn fresh_color_is_unchanged() {
        let yellow = AgentStatus::Working.color();
        assert_eq!(stale_color(yellow, 0), yellow);
        assert_eq!(stale_color(yellow, FRESH_SECS), yellow);
    }

    /// A fully stale color keeps its hue but drops to [`STALE_SATURATION`] of its
    /// original saturation.
    #[test]
    fn fully_stale_desaturates_but_keeps_hue() {
        let yellow = AgentStatus::Working.color();
        let faded = stale_color(yellow, STALE_SECS);
        assert_ne!(faded, yellow);

        let before = egui::ecolor::Hsva::from(yellow);
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
        let yellow = AgentStatus::Working.color();
        let mid_age = (FRESH_SECS + STALE_SECS) / 2;
        let mid = egui::ecolor::Hsva::from(stale_color(yellow, mid_age)).s;
        let fresh = egui::ecolor::Hsva::from(yellow).s;
        let stale = egui::ecolor::Hsva::from(stale_color(yellow, STALE_SECS)).s;
        assert!(stale < mid && mid < fresh, "{stale} < {mid} < {fresh}");
    }
}
