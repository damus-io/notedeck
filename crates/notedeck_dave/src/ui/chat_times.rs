//! When a chat's turns happened, drawn around them: a centred **Today** 9:41 AM
//! header above a turn that starts a new day or follows an hour's quiet, and
//! one "8m ago · worked 12m" footer under each finished turn.
//!
//! Placement and wording are worked out here, once, when the chat's rows, its
//! turns, the local day or the locale change ([`ChatTimes::refresh`]); the
//! chat view only reads the prepared text each frame. The one part that moves
//! with the clock is a recent footer's "8m ago", which is re-texted when its
//! relative bucket changes, not every frame.

use std::sync::Arc;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone};
use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, RichText, TextFormat, WidgetText};
use notedeck::fonts::NamedFontFamily;
use notedeck::{relative_time, tr, Localization, RelativeTime, RelativeTimeCache};

use crate::messages::Message;
use crate::session::TurnSpan;

/// A footer reads relative ("8m ago") until its turn is this old, then a clock
/// time; and a gap this long since the previous timed row earns a header.
const HOUR: u64 = 60 * 60;
/// A turn shorter than this shows no "worked" duration.
const MIN_WORKED_SECS: u64 = 60;
const HEADER_SIZE: f32 = 11.5;
const FOOTER_SIZE: f32 = 10.5;

/// A header's place: above `row`, a user message starting a turn at `at`
/// (unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderAt {
    pub row: usize,
    pub at: u64,
}

/// A footer's place: under `row`, the last row of a turn that ended at `end`
/// (unix seconds) after working `worked_secs`, or `None` when that was under a
/// minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FooterAt {
    pub row: usize,
    pub end: u64,
    pub worked_secs: Option<u64>,
}

/// Whether `message` starts a turn: a user message, other than the reply a
/// permission prompt sends on the user's behalf.
fn starts_turn(message: &Message) -> bool {
    matches!(message, Message::User(user) if !user.permission_reply)
}

/// `t` (unix seconds) as a local time in `tz`.
fn local<Tz: TimeZone>(t: u64, tz: &Tz) -> Option<DateTime<Tz>> {
    tz.timestamp_opt(t as i64, 0).single()
}

/// The local date of `t` in `tz`.
fn local_date<Tz: TimeZone>(t: u64, tz: &Tz) -> Option<NaiveDate> {
    local(t, tz).map(|t| t.date_naive())
}

/// The rows that get a header: each user message starting a turn on a new
/// local day, or an hour or more after the previous row with a time. Rows
/// without a time are passed over, both for the gap and for the header.
pub fn header_rows<Tz: TimeZone>(chat: &[Message], tz: &Tz) -> Vec<HeaderAt> {
    let mut headers = Vec::new();
    let mut prev: Option<u64> = None;
    for (row, message) in chat.iter().enumerate() {
        let Some(at) = crate::session::row_time(message) else {
            continue;
        };
        let new_day = prev.is_none_or(|p| local_date(p, tz) != local_date(at, tz));
        let quiet = prev.is_none_or(|p| at.saturating_sub(p) >= HOUR);
        if starts_turn(message) && (new_day || quiet) {
            headers.push(HeaderAt { row, at });
        }
        prev = Some(at);
    }
    headers
}

/// The rows that get a footer: the last row of each finished turn in `turns`,
/// for a chat of `rows` rows. Turns ending on the same row (a wake-up turn
/// that drew nothing) share one footer, from the first's start to the last's
/// end.
pub fn footer_rows(rows: usize, turns: &[TurnSpan]) -> Vec<FooterAt> {
    let mut footers: Vec<FooterAt> = Vec::new();
    let mut start = 0;
    for span in turns.iter().filter(|span| span.last_row < rows) {
        match footers.last_mut() {
            Some(last) if last.row == span.last_row => last.end = span.end,
            _ => {
                start = span.start;
                footers.push(FooterAt {
                    row: span.last_row,
                    end: span.end,
                    worked_secs: None,
                });
            }
        }
        let last = footers.last_mut().expect("pushed above");
        let worked = last.end.saturating_sub(start);
        last.worked_secs = (worked >= MIN_WORKED_SECS).then_some(worked);
    }
    footers
}

/// A moment, split the way a header draws it: a bold day and a regular time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayClock {
    /// "Today", "Yesterday", "Monday" within the week, else "Mon, Oct 6" (with
    /// the year when it isn't this one).
    pub day: String,
    /// "9:41 AM", or "at 9:41 AM" after a date.
    pub clock: String,
    /// The day is today, so a footer can leave it out.
    pub today: bool,
}

/// How a header names `t`, seen at `now` (both unix seconds), in `tz`.
pub fn day_clock<Tz: TimeZone>(t: u64, now: u64, tz: &Tz, i18n: &mut Localization) -> DayClock
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(at), Some(today)) = (local(t, tz), local_date(now, tz)) else {
        return DayClock {
            day: String::new(),
            clock: String::new(),
            today: false,
        };
    };
    let clock = at.format("%-I:%M %p").to_string();
    let days_ago = (today - at.date_naive()).num_days();
    let (day, clock) = match days_ago {
        0 => (
            tr!(
                i18n,
                "Today",
                "Dave chat: the day a turn header names, for today"
            ),
            clock,
        ),
        1 => (
            tr!(
                i18n,
                "Yesterday",
                "Dave chat: the day a turn header names, for yesterday"
            ),
            clock,
        ),
        2..=6 => (at.format("%A").to_string(), clock),
        _ => {
            let date = if at.year() == today.year() {
                at.format("%a, %b %-d").to_string()
            } else {
                at.format("%a, %b %-d, %Y").to_string()
            };
            let clock = tr!(
                i18n,
                "at {time}",
                "Dave chat: the time after a full date in a turn header, as in 'Mon, Oct 6 at 4:12 PM'",
                time = clock.as_str()
            );
            (date, clock)
        }
    };
    DayClock {
        day,
        clock,
        today: days_ago == 0,
    }
}

/// A turn's length: "12m", "1h 5m", "2h".
pub fn duration_text(secs: u64) -> String {
    let (hours, mins) = (secs / HOUR, (secs % HOUR) / 60);
    match (hours, mins) {
        (0, mins) => format!("{mins}m"),
        (hours, 0) => format!("{hours}h"),
        (hours, mins) => format!("{hours}h {mins}m"),
    }
}

/// A footer's "when", for a turn that ended under an hour ago: "8m ago", or
/// "now" for one that just did.
fn ago_text(relative: RelativeTime, rendered: &str, i18n: &mut Localization) -> String {
    if relative.first.is_none() {
        return rendered.to_owned();
    }
    tr!(
        i18n,
        "{time} ago",
        "Dave chat: when a turn ended, in its footer, as in '8m ago'",
        time = rendered
    )
}

/// What a footer's "when" shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// Under an hour: relative, in this bucket.
    Relative(RelativeTime),
    /// An hour or more: the clock time.
    Clock,
}

impl When {
    fn at(end: u64, now: u64) -> Self {
        if now.saturating_sub(end) < HOUR {
            When::Relative(relative_time(end, now))
        } else {
            When::Clock
        }
    }
}

/// One header's text, and the label it last drew as.
struct Header {
    row: usize,
    day: String,
    clock: String,
    /// Built on first draw and whenever the text colour changes.
    job: Option<Drawn<LayoutJob>>,
}

/// One footer's parts, and the label it last drew as.
struct Footer {
    at: FooterAt,
    /// The "when" once the turn is an hour old: "9:53 AM", "Yesterday 4:13 PM".
    clock: String,
    /// "worked 12m", for a turn of a minute or more.
    worked: Option<String>,
    /// Built when the "when" or the colour changes.
    text: Option<(When, Drawn<RichText>)>,
}

/// A label built for one text colour, shared with egui by `Arc` so drawing it
/// again clones nothing.
struct Drawn<T> {
    color: Color32,
    text: Arc<T>,
}

/// What the headers and footers were worked out from: when any of it changes
/// they are worked out again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seed {
    rows: usize,
    turns: usize,
    /// The locale's cache generation, so a language switch re-words them.
    locale: u64,
    /// When the local day they were named on ends ("Today" turns into
    /// "Yesterday"), in unix seconds.
    day_ends: u64,
}

/// A chat's turn headers and footers, ready to draw. Lives beside the chat on
/// its session and is reseeded only when the chat's rows, its turns, the local
/// day or the locale change, so a frame formats nothing.
#[derive(Default)]
pub struct ChatTimes {
    seed: Option<Seed>,
    /// By row, ascending.
    headers: Vec<Header>,
    /// By row, ascending.
    footers: Vec<Footer>,
}

/// What [`ChatTimes::refresh`] works from.
pub struct TimesInput<'a, Tz> {
    pub chat: &'a [Message],
    pub turns: &'a [TurnSpan],
    /// Unix seconds.
    pub now: u64,
    pub tz: &'a Tz,
}

impl ChatTimes {
    /// Work the headers and footers out again if the chat's rows, its turns,
    /// the local day or the locale changed since they last were.
    pub fn refresh<Tz: TimeZone>(&mut self, input: TimesInput<'_, Tz>, i18n: &mut Localization)
    where
        Tz::Offset: std::fmt::Display,
    {
        let TimesInput {
            chat,
            turns,
            now,
            tz,
        } = input;
        let fresh = self.seed.is_some_and(|seed| {
            seed.rows == chat.len()
                && seed.turns == turns.len()
                && seed.locale == i18n.cache_generation()
                && now < seed.day_ends
        });
        if fresh {
            return;
        }

        self.headers = header_rows(chat, tz)
            .into_iter()
            .map(|at| {
                let DayClock { day, clock, .. } = day_clock(at.at, now, tz, i18n);
                Header {
                    row: at.row,
                    day,
                    clock,
                    job: None,
                }
            })
            .collect();
        self.footers = footer_rows(chat.len(), turns)
            .into_iter()
            .map(|at| {
                let when = day_clock(at.end, now, tz, i18n);
                let clock = if when.today {
                    when.clock
                } else {
                    format!("{} {}", when.day, when.clock)
                };
                let worked = at.worked_secs.map(|secs| {
                    let duration = duration_text(secs);
                    tr!(
                        i18n,
                        "worked {duration}",
                        "Dave chat: how long a turn ran, in its footer, as in 'worked 12m'",
                        duration = duration.as_str()
                    )
                });
                Footer {
                    at,
                    clock,
                    worked,
                    text: None,
                }
            })
            .collect();
        self.seed = Some(Seed {
            rows: chat.len(),
            turns: turns.len(),
            locale: i18n.cache_generation(),
            day_ends: day_end(now, tz),
        });
    }

    /// The header above `row`, if it has one, drawn in `color`.
    pub fn header(&mut self, row: usize, color: Color32) -> Option<WidgetText> {
        let i = self.headers.binary_search_by_key(&row, |h| h.row).ok()?;
        let header = &mut self.headers[i];
        if header.job.as_ref().is_none_or(|job| job.color != color) {
            header.job = Some(Drawn {
                color,
                text: Arc::new(header_job(&header.day, &header.clock, color)),
            });
        }
        let job = header.job.as_ref().expect("built above");
        Some(WidgetText::LayoutJob(job.text.clone()))
    }

    /// The footer under `row`, if it has one, drawn in `color` as of `now`
    /// (unix seconds). Re-texted only when its relative bucket ("8m" to
    /// "9m"), its switch to a clock time, or the colour changes.
    pub fn footer(
        &mut self,
        row: usize,
        now: u64,
        color: Color32,
        time_cache: &mut RelativeTimeCache,
        i18n: &mut Localization,
    ) -> Option<WidgetText> {
        let i = self.footers.binary_search_by_key(&row, |f| f.at.row).ok()?;
        let footer = &mut self.footers[i];
        let when = When::at(footer.at.end, now);
        let stale = footer
            .text
            .as_ref()
            .is_none_or(|(shown, drawn)| *shown != when || drawn.color != color);
        if stale {
            let when_text = match when {
                When::Relative(relative) => {
                    let rendered = time_cache.time_ago_between(i18n, footer.at.end, now);
                    ago_text(relative, rendered, i18n)
                }
                When::Clock => footer.clock.clone(),
            };
            let text = match &footer.worked {
                Some(worked) => format!("{when_text} · {worked}"),
                None => when_text,
            };
            let text = RichText::new(text).size(FOOTER_SIZE).color(color);
            footer.text = Some((
                when,
                Drawn {
                    color,
                    text: Arc::new(text),
                },
            ));
        }
        let (_, drawn) = footer.text.as_ref().expect("built above");
        Some(WidgetText::RichText(drawn.text.clone()))
    }

    /// Seconds until a label drawn at `now` (unix seconds) next changes: the
    /// newest footer while it is under an hour old, or the running turn's
    /// "· 3m" when one started at `working_since`. `None` when nothing moves.
    pub fn next_change(&self, now: u64, working_since: Option<u64>) -> Option<u64> {
        let newest_footer = self
            .footers
            .last()
            .map(|footer| footer.at.end)
            .filter(|end| now.saturating_sub(*end) < HOUR);
        [newest_footer, working_since]
            .into_iter()
            .flatten()
            .map(|since| secs_to_next_bucket(now.saturating_sub(since)))
            .min()
    }
}

/// Seconds from an age of `age` seconds until its relative label changes: a
/// second while it counts seconds, then the next minute edge.
fn secs_to_next_bucket(age: u64) -> u64 {
    if age < 60 {
        1
    } else {
        60 - age % 60
    }
}

/// When the local day holding `now` ends, in unix seconds. An hour on if
/// local midnight doesn't exist (a DST jump), so a missing one costs a
/// reseed, not a stale "Today".
fn day_end<Tz: TimeZone>(now: u64, tz: &Tz) -> u64 {
    local_date(now, tz)
        .and_then(|today| today.succ_opt())
        .and_then(|tomorrow| tomorrow.and_hms_opt(0, 0, 0))
        .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
        .map(|midnight| midnight.timestamp() as u64)
        .unwrap_or(now + HOUR)
}

/// A header's label: the day in bold, the clock regular, both in `color`.
fn header_job(day: &str, clock: &str, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let bold = FontFamily::Name(NamedFontFamily::Bold.as_str().into());
    job.append(
        day,
        0.0,
        TextFormat {
            font_id: FontId::new(HEADER_SIZE, bold),
            color,
            ..Default::default()
        },
    );
    let regular = TextFormat {
        font_id: FontId::proportional(HEADER_SIZE),
        color,
        ..Default::default()
    };
    // A real space, so the label reads "Today 9:41 AM" to a screen reader.
    job.append(" ", 0.0, regular.clone());
    job.append(clock, 0.0, regular);
    job
}

/// Draw a turn header: centred, with room above and below.
pub fn header_ui(ui: &mut egui::Ui, text: WidgetText) {
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        ui.label(text);
    });
    ui.add_space(4.0);
}

/// Draw a turn footer: small, under the turn's last row, at its left.
pub fn footer_ui(ui: &mut egui::Ui, text: WidgetText) {
    ui.add_space(2.0);
    ui.label(text);
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, UserMessage};
    use chrono::Utc;

    /// Unix seconds for 2026-10-`day` `h`:`m` UTC.
    fn at(day: u32, h: u32, m: u32) -> u64 {
        NaiveDate::from_ymd_opt(2026, 10, day)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
            .and_utc()
            .timestamp() as u64
    }

    fn user(t: u64) -> Message {
        Message::User(UserMessage {
            sent_at: Some(t),
            ..UserMessage::from("hi")
        })
    }

    fn reply(t: u64) -> Message {
        Message::Assistant(AssistantMessage::new().with_sent_at(t))
    }

    fn tool() -> Message {
        Message::Error("a row with no time".into())
    }

    fn rows(headers: &[HeaderAt]) -> Vec<usize> {
        headers.iter().map(|h| h.row).collect()
    }

    /// The first timed user message gets a header; so does one on a new day
    /// or after an hour's quiet. One 59 minutes on does not, and a reply never
    /// does, even after an hour.
    #[test]
    fn headers_go_on_turns_after_a_new_day_or_an_hour() {
        let chat = [
            tool(),
            user(at(12, 16, 12)),
            reply(at(12, 16, 13)),
            user(at(13, 9, 41)),   // a new day
            reply(at(13, 9, 44)),  //
            tool(),                // no time: skipped
            reply(at(13, 10, 50)), // an hour on, but a reply
            user(at(13, 11, 49)),  // 59m after the reply
            user(at(13, 12, 49)),  // an hour after it
        ];
        assert_eq!(rows(&header_rows(&chat, &Utc)), [1, 3, 8]);
    }

    /// A permission prompt's reply is the user's row but no turn's start.
    #[test]
    fn a_permission_reply_starts_no_turn() {
        let chat = [Message::User(UserMessage {
            sent_at: Some(at(13, 9, 0)),
            ..UserMessage::permission_reply("allow")
        })];
        assert!(header_rows(&chat, &Utc).is_empty());
    }

    /// Each finished turn gets one footer under its last row, with its
    /// duration from a minute on; a turn past the chat's end gets none, and
    /// two ending on one row share one.
    #[test]
    fn footers_go_under_each_finished_turn() {
        let span = |last_row, start, end| TurnSpan {
            last_row,
            start,
            end,
        };
        let turns = [
            span(1, 1_000, 1_059),
            span(4, 2_000, 2_720),
            span(4, 3_000, 3_100),
            span(9, 4_000, 4_600),
        ];
        assert_eq!(
            footer_rows(5, &turns),
            [
                FooterAt {
                    row: 1,
                    end: 1_059,
                    worked_secs: None,
                },
                FooterAt {
                    row: 4,
                    end: 3_100,
                    worked_secs: Some(1_100),
                },
            ]
        );
    }

    #[test]
    fn a_header_names_the_day_then_the_time() {
        let mut i18n = Localization::no_bidi();
        let now = at(13, 10, 1);
        let mut named = |t| {
            let DayClock { day, clock, .. } = day_clock(t, now, &Utc, &mut i18n);
            format!("{day} {clock}")
        };
        assert_eq!(named(at(13, 9, 41)), "Today 9:41 AM");
        assert_eq!(named(at(12, 16, 12)), "Yesterday 4:12 PM");
        assert_eq!(named(at(8, 16, 12)), "Thursday 4:12 PM");
        assert_eq!(named(at(6, 16, 12)), "Tue, Oct 6 at 4:12 PM");
        let last_year = at(6, 16, 12) - 365 * 24 * HOUR;
        assert_eq!(named(last_year), "Mon, Oct 6, 2025 at 4:12 PM");
    }

    #[test]
    fn durations_read_in_hours_and_minutes() {
        assert_eq!(duration_text(60), "1m");
        assert_eq!(duration_text(12 * 60 + 59), "12m");
        assert_eq!(duration_text(2 * HOUR), "2h");
        assert_eq!(duration_text(HOUR + 5 * 60), "1h 5m");
    }

    fn footer_text(times: &mut ChatTimes, row: usize, now: u64) -> String {
        let mut cache = RelativeTimeCache::default();
        let mut i18n = Localization::no_bidi();
        let text = times
            .footer(row, now, Color32::GRAY, &mut cache, &mut i18n)
            .expect("a footer");
        text.text().to_owned()
    }

    fn seeded(chat: &[Message], turns: &[TurnSpan], now: u64) -> ChatTimes {
        let mut times = ChatTimes::default();
        let input = TimesInput {
            chat,
            turns,
            now,
            tz: &Utc,
        };
        times.refresh(input, &mut Localization::no_bidi());
        times
    }

    /// A footer reads "8m ago · worked 12m" for its first hour, then the
    /// clock time it ended at, with the day once that isn't today.
    #[test]
    fn a_footer_is_relative_for_an_hour_then_a_clock() {
        let chat = [user(at(13, 9, 41)), reply(at(13, 9, 53))];
        let turns = [TurnSpan {
            last_row: 1,
            start: at(13, 9, 41),
            end: at(13, 9, 53),
        }];
        let mut times = seeded(&chat, &turns, at(13, 10, 1));
        assert_eq!(
            footer_text(&mut times, 1, at(13, 10, 1)),
            "8m ago · worked 12m"
        );
        assert_eq!(
            footer_text(&mut times, 1, at(13, 9, 53)),
            "now · worked 12m"
        );
        assert_eq!(
            footer_text(&mut times, 1, at(13, 11, 0)),
            "9:53 AM · worked 12m"
        );

        let mut tomorrow = seeded(&chat, &turns, at(14, 8, 0));
        assert_eq!(
            footer_text(&mut tomorrow, 1, at(14, 8, 0)),
            "Yesterday 9:53 AM · worked 12m"
        );
        assert!(tomorrow.header(0, Color32::GRAY).is_some());
        assert!(tomorrow.header(1, Color32::GRAY).is_none());
    }

    /// The labels are worked out once a day: a later frame the same day keeps
    /// them, and the first one after midnight re-words "Today".
    #[test]
    fn a_new_day_reseeds_the_labels() {
        let chat = [user(at(13, 9, 41))];
        let mut times = seeded(&chat, &[], at(13, 10, 0));
        let seed = times.seed;
        let mut i18n = Localization::no_bidi();
        let input = |now| TimesInput {
            chat: &chat,
            turns: &[],
            now,
            tz: &Utc,
        };
        times.refresh(input(at(13, 23, 59)), &mut i18n);
        assert_eq!(times.seed, seed, "same day");
        times.refresh(input(at(14, 0, 0)), &mut i18n);
        assert_eq!(times.headers[0].day, "Yesterday");
    }

    /// Repaints come at the next label change, only while one can change.
    #[test]
    fn the_next_change_is_the_next_bucket() {
        let chat = [user(1_000), reply(1_100)];
        let turns = [TurnSpan {
            last_row: 1,
            start: 1_000,
            end: 1_100,
        }];
        let times = seeded(&chat, &turns, 1_100);
        assert_eq!(times.next_change(1_130, None), Some(1), "counting seconds");
        assert_eq!(times.next_change(1_100 + 125, None), Some(55));
        assert_eq!(times.next_change(1_100 + HOUR, None), None, "a clock now");
        assert_eq!(
            times.next_change(1_100 + HOUR, Some(1_100 + HOUR - 90)),
            Some(30)
        );
    }
}
