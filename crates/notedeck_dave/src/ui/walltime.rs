//! The session header's walltime: how long the session has spent working,
//! "· 1h 12m" after its model and cwd.
//!
//! Formatted only when the text it shows would change, so a frame in between
//! allocates nothing: [`WalltimeLabel`] lives on the session beside its chat
//! and keeps the built label until the shown value, the turn count, the
//! colour or the locale moves.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText, WidgetText};
use notedeck::{tr_plural, Localization};

use crate::session::Walltime;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// The subtitle's font size, which the walltime matches.
const SUBTITLE_SIZE: f32 = 10.0;

/// A walltime: "45s" under a minute, then "12m", "1h 12m", and "2d 3h" from a
/// day on.
pub fn walltime_text(secs: u64) -> String {
    if secs < MINUTE {
        return format!("{secs}s");
    }
    let (big, small, big_unit, small_unit) = if secs < DAY {
        (secs / HOUR, (secs % HOUR) / MINUTE, "h", "m")
    } else {
        (secs / DAY, (secs % DAY) / HOUR, "d", "h")
    };
    match (big, small) {
        (0, small) => format!("{small}{small_unit}"),
        (big, 0) => format!("{big}{big_unit}"),
        (big, small) => format!("{big}{big_unit} {small}{small_unit}"),
    }
}

/// The finest unit [`walltime_text`] shows for `secs`, and how many of it:
/// the text changes exactly when this does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Secs(u64),
    Mins(u64),
    Hours(u64),
}

impl Shown {
    fn of(secs: u64) -> Self {
        if secs < MINUTE {
            Shown::Secs(secs)
        } else if secs < DAY {
            Shown::Mins(secs / MINUTE)
        } else {
            Shown::Hours(secs / HOUR)
        }
    }
}

/// Seconds from a walltime of `secs` until its text next changes: a second
/// while it counts seconds, the next minute edge under a day, then the next
/// hour edge.
fn secs_to_next_change(secs: u64) -> u64 {
    if secs < MINUTE {
        1
    } else if secs < DAY {
        MINUTE - secs % MINUTE
    } else {
        HOUR - secs % HOUR
    }
}

/// What the label was built from: when any of it changes it is built again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    shown: Shown,
    turns: usize,
    color: Color32,
    /// The locale's cache generation, so a language switch re-words the
    /// hover.
    locale: u64,
}

/// A built walltime label and its hover, shared with egui by `Arc` so
/// drawing them again clones nothing.
struct Built {
    key: Key,
    text: Arc<LayoutJob>,
    hover: Arc<RichText>,
}

/// What [`WalltimeLabel::refresh`] works from.
pub struct WalltimeInput {
    pub walltime: Walltime,
    /// The session's finished turns.
    pub finished_turns: usize,
    /// Unix seconds.
    pub now: u64,
    /// The subtitle's (weak) text colour, for the separator and an idle
    /// number.
    pub weak: Color32,
    /// The number's colour while a turn runs, so it reads as live.
    pub live: Color32,
}

/// The header's walltime, ready to draw. Hidden while the session has worked
/// no time at all, as a fresh one hasn't.
#[derive(Default)]
pub struct WalltimeLabel {
    built: Option<Built>,
}

/// A walltime label to draw in the header subtitle, with its hover.
pub struct HeaderWalltime {
    /// "· 1h 12m", the separator weak and the number tinted while working.
    pub text: WidgetText,
    /// "worked 1h 12m over 9 turns".
    pub hover: WidgetText,
}

impl WalltimeLabel {
    /// Rebuild the label if what it shows changed since it last was built.
    pub fn refresh(&mut self, input: WalltimeInput, i18n: &mut Localization) {
        let WalltimeInput {
            walltime,
            finished_turns,
            now,
            weak,
            live,
        } = input;
        let secs = walltime.total_at(now);
        if secs == 0 {
            self.built = None;
            return;
        }
        let running = walltime.working_since.is_some();
        let key = Key {
            shown: Shown::of(secs),
            turns: finished_turns + usize::from(running),
            color: if running { live } else { weak },
            locale: i18n.cache_generation(),
        };
        if self.built.as_ref().is_some_and(|built| built.key == key) {
            return;
        }

        let text = walltime_text(secs);
        let hover = tr_plural!(
            i18n,
            "worked {duration} over {count} turn",
            "worked {duration} over {count} turns",
            "Dave session header: hover over the session's walltime, as in 'worked 1h 12m over 9 turns'",
            key.turns as i64,
            duration = text.as_str()
        );
        self.built = Some(Built {
            key,
            text: Arc::new(walltime_job(&text, weak, key.color)),
            hover: Arc::new(RichText::new(hover)),
        });
    }

    /// The label as last refreshed, or `None` while it is hidden.
    pub fn label(&self) -> Option<HeaderWalltime> {
        self.built.as_ref().map(|built| HeaderWalltime {
            text: WidgetText::LayoutJob(built.text.clone()),
            hover: WidgetText::RichText(built.hover.clone()),
        })
    }

    /// Seconds until the label drawn at `now` (unix seconds) next changes,
    /// or `None` when nothing runs to change it.
    pub fn next_change(walltime: Walltime, now: u64) -> Option<u64> {
        walltime.working_since?;
        Some(secs_to_next_change(walltime.total_at(now)))
    }
}

/// The label: a weak "· " then the number in `color`, both monospace like
/// the rest of the subtitle.
fn walltime_job(text: &str, weak: Color32, color: Color32) -> LayoutJob {
    let font_id = FontId::monospace(SUBTITLE_SIZE);
    let mut job = LayoutJob::default();
    job.append(
        "\u{00B7} ",
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: weak,
            ..Default::default()
        },
    );
    job.append(
        text,
        0.0,
        TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walltimes_read_in_their_two_largest_units() {
        assert_eq!(walltime_text(45), "45s");
        assert_eq!(walltime_text(MINUTE), "1m");
        assert_eq!(walltime_text(12 * MINUTE + 59), "12m");
        assert_eq!(walltime_text(HOUR + 12 * MINUTE), "1h 12m");
        assert_eq!(walltime_text(2 * HOUR), "2h");
        assert_eq!(walltime_text(23 * HOUR + 59 * MINUTE + 59), "23h 59m");
        assert_eq!(walltime_text(2 * DAY + 3 * HOUR + 59 * MINUTE), "2d 3h");
        assert_eq!(walltime_text(DAY), "1d");
    }

    #[test]
    fn the_text_changes_exactly_when_the_shown_unit_does() {
        for secs in [0, 59, 60, 61, 3599, 3600, DAY - 1, DAY, DAY + HOUR - 1] {
            let next = secs + secs_to_next_change(secs);
            assert_ne!(walltime_text(secs), walltime_text(next), "at {secs}");
            assert_eq!(
                walltime_text(secs),
                walltime_text(next - 1),
                "nothing changes before {next}"
            );
            assert_ne!(Shown::of(secs), Shown::of(next));
        }
    }

    fn input(walltime: Walltime, finished_turns: usize, now: u64) -> WalltimeInput {
        WalltimeInput {
            walltime,
            finished_turns,
            now,
            weak: Color32::GRAY,
            live: Color32::YELLOW,
        }
    }

    fn text(label: &WalltimeLabel) -> Option<String> {
        label.label().map(|l| l.text.text().to_owned())
    }

    #[test]
    fn a_session_that_has_not_worked_shows_nothing() {
        let mut i18n = Localization::no_bidi();
        let mut label = WalltimeLabel::default();
        label.refresh(input(Walltime::default(), 0, 1_000), &mut i18n);
        assert_eq!(text(&label), None);
    }

    #[test]
    fn a_running_turn_counts_up_and_names_its_turns() {
        let mut i18n = Localization::no_bidi();
        let mut label = WalltimeLabel::default();
        let walltime = Walltime {
            worked_secs: HOUR,
            working_since: Some(1_000),
        };
        label.refresh(input(walltime, 8, 1_000 + 12 * MINUTE), &mut i18n);
        assert_eq!(text(&label).as_deref(), Some("· 1h 12m"));
        let hover = label.label().expect("shown").hover.text().to_owned();
        assert_eq!(hover, "worked 1h 12m over 9 turns");
        assert_eq!(
            WalltimeLabel::next_change(walltime, 1_000 + 12 * MINUTE + 20),
            Some(40)
        );

        let idle = Walltime {
            working_since: None,
            ..walltime
        };
        assert_eq!(WalltimeLabel::next_change(idle, 5_000), None);
    }

    #[test]
    fn the_label_is_built_once_per_shown_value() {
        let mut i18n = Localization::no_bidi();
        let mut label = WalltimeLabel::default();
        let walltime = Walltime {
            worked_secs: 0,
            working_since: Some(0),
        };
        label.refresh(input(walltime, 0, 90), &mut i18n);
        let first = label.built.as_ref().map(|b| Arc::as_ptr(&b.text));
        label.refresh(input(walltime, 0, 119), &mut i18n);
        let same = label.built.as_ref().map(|b| Arc::as_ptr(&b.text));
        assert_eq!(first, same, "still 1m: kept");
        label.refresh(input(walltime, 0, 120), &mut i18n);
        assert_eq!(text(&label).as_deref(), Some("· 2m"));
    }
}
