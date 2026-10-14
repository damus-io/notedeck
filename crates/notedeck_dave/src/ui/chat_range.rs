//! A range of chat rows picked to see how long they took: Shift+click a
//! message to stretch a range to it from an anchor, and a pill under the
//! range's last row reads "9:41 AM → 9:53 AM · 12m · 6 messages".
//!
//! The range is over chat rows (indices into the chat), not the block
//! cursor's blocks: most messages aren't blocks. Its anchor is the block
//! cursor's row while there is a cursor, else the last message plainly
//! clicked. The pill's text is worked out when the range, the chat's rows,
//! the locale or the local day change ([`ChatRange::refresh`]), so a frame
//! formats nothing.

use std::ops::RangeInclusive;
use std::sync::Arc;

use chrono::TimeZone;
use egui::{Color32, RichText, WidgetText};
use notedeck::{tr_plural, Localization};

use crate::messages::Message;

use super::chat_times::{day_clock, duration_text, DayClock};

/// A range shorter than this shows no duration, like a turn's footer.
const MIN_DURATION_SECS: u64 = 60;
const PILL_SIZE: f32 = 11.0;

/// The two ends of a range: where it was started from and where it was
/// stretched to. Either can be the earlier row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub anchor: usize,
    pub head: usize,
}

impl Span {
    /// The rows it covers, earliest first.
    pub fn rows(self) -> RangeInclusive<usize> {
        self.anchor.min(self.head)..=self.anchor.max(self.head)
    }
}

/// Which way Shift+j / Shift+k stretch a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Down,
    Up,
}

/// What a range covers, for its pill: when its first and last timed rows
/// were sent (unix seconds), and how many messages (user and assistant rows;
/// tool calls and notices aren't messages) it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSummary {
    /// The first timed row's time, `None` when no row in it has one.
    pub first: Option<u64>,
    /// The last timed row's time; equal to `first` with one timed row.
    pub last: Option<u64>,
    pub messages: usize,
}

/// What the pill's text was worked out from: when any of it changes it is
/// worked out again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seed {
    span: Span,
    rows: usize,
    locale: u64,
    /// When the local day it was named on ends, so "Yesterday" isn't left
    /// reading "Today".
    day_ends: u64,
    color: Color32,
    weak: Color32,
}

/// The pill's labels as last worked out.
struct Pill {
    seed: Seed,
    text: Arc<RichText>,
    copy: Arc<RichText>,
    close: Arc<RichText>,
}

/// What a range's pill draws: its summary, and its Copy and × buttons'
/// labels. Shared with the cache by `Arc`, so a frame clones nothing.
pub struct PillLabels {
    pub text: WidgetText,
    pub copy: WidgetText,
    pub close: WidgetText,
}

/// A chat's message range and its pill. Lives beside the chat on its session.
#[derive(Default)]
pub struct ChatRange {
    /// The message last plainly clicked: a Shift+click with no block cursor
    /// stretches a range from it.
    clicked: Option<usize>,
    span: Option<Span>,
    pill: Option<Pill>,
}

/// What [`ChatRange::refresh`] works from.
pub struct RangeInput<'a, Tz> {
    pub chat: &'a [Message],
    /// Unix seconds.
    pub now: u64,
    pub tz: &'a Tz,
    /// The summary's colour.
    pub color: Color32,
    /// The buttons' colour.
    pub weak: Color32,
}

impl ChatRange {
    /// The range, if one is picked.
    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// A message was clicked without Shift: it is where the next range
    /// starts, and any range there is goes.
    pub fn click(&mut self, row: usize) {
        self.clicked = Some(row);
        self.span = None;
    }

    /// A message was Shift+clicked: stretch the range to `row`, from the
    /// range's own anchor if there is one, else the block cursor's row
    /// (`cursor_row`), else the last plainly clicked message, else `row`
    /// itself.
    pub fn extend_to(&mut self, row: usize, cursor_row: Option<usize>) {
        let anchor = self
            .span
            .map(|span| span.anchor)
            .or(cursor_row)
            .or(self.clicked)
            .unwrap_or(row);
        self.span = Some(Span { anchor, head: row });
    }

    /// Stretch the range a row further `step`, skipping rows the chat
    /// doesn't draw. With no range it starts at the block cursor's row
    /// (`cursor_row`); with neither it does nothing.
    pub fn step(
        &mut self,
        step: Step,
        cursor_row: Option<usize>,
        chat: &[Message],
        drawn: impl Fn(&Message) -> bool,
    ) {
        let Some(span) = self.span.or(cursor_row.map(|row| Span {
            anchor: row,
            head: row,
        })) else {
            return;
        };
        let next = match step {
            Step::Down => (span.head + 1..chat.len()).find(|&row| drawn(&chat[row])),
            Step::Up => (0..span.head).rev().find(|&row| drawn(&chat[row])),
        };
        self.span = Some(Span {
            anchor: span.anchor,
            head: next.unwrap_or(span.head),
        });
    }

    /// Drop the range (Esc, or the pill's ×). The anchor a plain click left
    /// stays.
    pub fn clear(&mut self) {
        self.span = None;
    }

    /// Drop a range that runs past a chat of `rows` rows, as after the chat
    /// was replaced with a shorter one.
    pub fn fit(&mut self, rows: usize) {
        if self.span.is_some_and(|span| *span.rows().end() >= rows) {
            self.span = None;
        }
        if self.clicked.is_some_and(|row| row >= rows) {
            self.clicked = None;
        }
    }

    /// Work the pill's text out again if the range, the chat's rows, the
    /// locale, the local day or the colour changed since it last was.
    pub fn refresh<Tz: TimeZone>(&mut self, input: RangeInput<'_, Tz>, i18n: &mut Localization)
    where
        Tz::Offset: std::fmt::Display,
    {
        let RangeInput {
            chat,
            now,
            tz,
            color,
            weak,
        } = input;
        self.fit(chat.len());
        let Some(span) = self.span else {
            self.pill = None;
            return;
        };
        let fresh = self.pill.as_ref().is_some_and(|pill| {
            pill.seed.span == span
                && pill.seed.rows == chat.len()
                && pill.seed.locale == i18n.cache_generation()
                && pill.seed.color == color
                && pill.seed.weak == weak
                && now < pill.seed.day_ends
        });
        if fresh {
            return;
        }
        let summary = summarize(chat, span.rows());
        let text = pill_text(summary, now, tz, i18n);
        self.pill = Some(Pill {
            seed: Seed {
                span,
                rows: chat.len(),
                locale: i18n.cache_generation(),
                day_ends: super::chat_times::day_end(now, tz),
                color,
                weak,
            },
            text: Arc::new(RichText::new(text).size(PILL_SIZE).color(color)),
            copy: Arc::new(RichText::new("Copy").size(PILL_SIZE).color(weak)),
            close: Arc::new(RichText::new("×").size(PILL_SIZE).color(weak)),
        });
    }

    /// The pill's labels as last refreshed, `None` with no range.
    pub fn pill(&self) -> Option<PillLabels> {
        let pill = self.pill.as_ref()?;
        Some(PillLabels {
            text: WidgetText::RichText(pill.text.clone()),
            copy: WidgetText::RichText(pill.copy.clone()),
            close: WidgetText::RichText(pill.close.clone()),
        })
    }
}

/// When the first and last timed rows of `rows` in `chat` were sent, and
/// how many messages they hold.
pub fn summarize(chat: &[Message], rows: RangeInclusive<usize>) -> RangeSummary {
    let in_range = || chat.get(rows.clone()).into_iter().flatten();
    let mut times = in_range().filter_map(crate::session::row_time);
    let first = times.next();
    let last = times.last().or(first);
    RangeSummary {
        first,
        last,
        messages: in_range().filter(|m| is_message(m)).count(),
    }
}

/// Whether `message` is one a person or the assistant wrote: what a range
/// counts, and copies.
fn is_message(message: &Message) -> bool {
    matches!(message, Message::User(_) | Message::Assistant(_))
}

/// The pill's text for `summary`, seen at `now` in `tz`: "9:41 AM → 9:53 AM
/// · 12m · 6 messages". A day goes before a start that isn't today, and
/// before the end when it isn't the start's day. One timed row shows
/// one time, none shows only the count, and under a minute shows no
/// duration.
pub fn pill_text<Tz: TimeZone>(
    summary: RangeSummary,
    now: u64,
    tz: &Tz,
    i18n: &mut Localization,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let count = tr_plural!(
        i18n,
        "{count} message",
        "{count} messages",
        "Dave chat: how many messages a picked range holds, in its pill, as in '6 messages'",
        summary.messages as i64,
    );
    let (Some(first), Some(last)) = (summary.first, summary.last) else {
        return count;
    };
    let start = day_clock(first, now, tz, i18n);
    let mut text = when(&start);
    if last != first {
        let end = day_clock(last, now, tz, i18n);
        let end = if end.day == start.day {
            end.clock
        } else {
            format!("{} {}", end.day, end.clock)
        };
        text.push_str(" → ");
        text.push_str(&end);
    }
    let secs = last.saturating_sub(first);
    if secs >= MIN_DURATION_SECS {
        text.push_str(" · ");
        text.push_str(&duration_text(secs));
    }
    text.push_str(" · ");
    text.push_str(&count);
    text
}

/// A moment as a pill names it: the clock alone today, else the day first.
fn when(at: &DayClock) -> String {
    if at.today {
        at.clock.clone()
    } else {
        format!("{} {}", at.day, at.clock)
    }
}

/// The text Copy puts on the clipboard for `rows` of `chat`: each message's
/// text, as its own right-click Copy gives it, joined with blank lines.
/// Rows with no text of their own (tool calls, notices) are left out.
pub fn copy_text(chat: &[Message], rows: RangeInclusive<usize>) -> String {
    let texts = chat
        .get(rows)
        .into_iter()
        .flatten()
        .filter_map(|message| match message {
            Message::User(user) => Some(user.text.as_str()),
            Message::Assistant(reply) => Some(reply.text()),
            _ => None,
        });
    let mut out = String::new();
    for text in texts {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(text);
    }
    out
}

/// What the pill under a range was asked to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillAction {
    Copy,
    Clear,
}

/// Draw the pill under a range: its text, then Copy and ×.
pub fn pill_ui(ui: &mut egui::Ui, labels: PillLabels) -> Option<PillAction> {
    let PillLabels { text, copy, close } = labels;
    let mut action = None;
    ui.add_space(2.0);
    egui::Frame::new()
        .fill(super::dave::block_cursor_fill(ui))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(notedeck::tokens::RADIUS_LG)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(text);
                if ui.add(egui::Button::new(copy).frame(false)).clicked() {
                    action = Some(PillAction::Copy);
                }
                let close = ui.add(egui::Button::new(close).frame(false));
                if close.on_hover_text("Esc").clicked() {
                    action = Some(PillAction::Clear);
                }
            });
        });
    ui.add_space(4.0);
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, UserMessage};
    use chrono::Utc;

    fn at(day: u32, h: u32, m: u32) -> u64 {
        chrono::NaiveDate::from_ymd_opt(2026, 10, day)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
            .and_utc()
            .timestamp() as u64
    }

    fn user(t: u64) -> Message {
        Message::User(UserMessage {
            sent_at: Some(t),
            ..UserMessage::from("go")
        })
    }

    fn reply(t: u64) -> Message {
        Message::Assistant(AssistantMessage::from_text("done".to_owned()).with_sent_at(t))
    }

    fn untimed() -> Message {
        Message::Assistant(AssistantMessage::from_text("thinking".to_owned()))
    }

    fn todo() -> Message {
        Message::TodoUpdate(serde_json::Value::Null)
    }

    fn drawn(m: &Message) -> bool {
        !matches!(m, Message::TodoUpdate(_))
    }

    #[test]
    fn a_shift_click_stretches_from_the_cursor_then_the_click() {
        let mut range = ChatRange::default();
        // With nothing to start from, the range is the one row.
        range.extend_to(3, None);
        assert_eq!(range.span().unwrap().rows(), 3..=3);

        // A plain click drops it and anchors the next one.
        range.click(5);
        assert_eq!(range.span(), None);
        range.extend_to(2, None);
        assert_eq!(range.span().unwrap().rows(), 2..=5);
        // Stretching again keeps the anchor, either way.
        range.extend_to(8, None);
        assert_eq!(range.span().unwrap().rows(), 5..=8);

        // The block cursor's row outranks the plain click.
        range.clear();
        range.extend_to(9, Some(1));
        assert_eq!(range.span().unwrap().rows(), 1..=9);
    }

    #[test]
    fn a_step_stretches_a_row_either_way_past_hidden_rows() {
        let chat = [user(1), reply(2), todo(), reply(3), user(4)];
        let mut range = ChatRange::default();
        // Nothing to step from.
        range.step(Step::Down, None, &chat, drawn);
        assert_eq!(range.span(), None);

        range.step(Step::Down, Some(1), &chat, drawn);
        assert_eq!(range.span().unwrap().rows(), 1..=3, "skips the todo");
        range.step(Step::Down, Some(1), &chat, drawn);
        range.step(Step::Down, Some(1), &chat, drawn);
        assert_eq!(range.span().unwrap().rows(), 1..=4, "stops at the end");
        for _ in 0..4 {
            range.step(Step::Up, None, &chat, drawn);
        }
        assert_eq!(range.span().unwrap().rows(), 0..=1, "past the anchor");
    }

    #[test]
    fn a_range_past_a_shorter_chat_goes() {
        let mut range = ChatRange::default();
        range.click(1);
        range.extend_to(6, None);
        range.fit(7);
        assert!(range.span().is_some());
        range.fit(4);
        assert_eq!(range.span(), None);
    }

    #[test]
    fn a_summary_takes_the_first_and_last_timed_rows() {
        let chat = [
            untimed(),
            user(at(13, 9, 41)),
            untimed(),
            todo(),
            reply(at(13, 9, 53)),
            untimed(),
        ];
        assert_eq!(
            summarize(&chat, 0..=5),
            RangeSummary {
                first: Some(at(13, 9, 41)),
                last: Some(at(13, 9, 53)),
                messages: 5,
            },
            "the todo isn't a message"
        );
        let one = summarize(&chat, 1..=3);
        assert_eq!(
            (one.first, one.last),
            (Some(at(13, 9, 41)), Some(at(13, 9, 41)))
        );
        let none = summarize(&chat, 2..=3);
        assert_eq!((none.first, none.last, none.messages), (None, None, 1));
    }

    #[test]
    fn the_pill_reads_times_a_duration_and_a_count() {
        let mut i18n = Localization::no_bidi();
        let now = at(13, 10, 0);
        let pill = |summary, i18n: &mut Localization| pill_text(summary, now, &Utc, i18n);
        let summary = |first, last, messages| RangeSummary {
            first,
            last,
            messages,
        };

        let long = summary(Some(at(13, 9, 41)), Some(at(13, 9, 53)), 6);
        assert_eq!(
            pill(long, &mut i18n),
            "9:41 AM → 9:53 AM · 12m · 6 messages"
        );

        // Under a minute: no duration.
        let quick = summary(Some(at(13, 9, 41)), Some(at(13, 9, 41) + 30), 2);
        assert_eq!(pill(quick, &mut i18n), "9:41 AM → 9:41 AM · 2 messages");

        let one = summary(Some(at(13, 9, 41)), Some(at(13, 9, 41)), 1);
        assert_eq!(pill(one, &mut i18n), "9:41 AM · 1 message");

        let untimed = summary(None, None, 3);
        assert_eq!(pill(untimed, &mut i18n), "3 messages");

        // Across days: each end names its day.
        let overnight = summary(Some(at(12, 16, 12)), Some(at(13, 9, 53)), 10);
        assert_eq!(
            pill(overnight, &mut i18n),
            "Yesterday 4:12 PM → Today 9:53 AM · 17h 41m · 10 messages"
        );
    }

    #[test]
    fn copy_joins_the_messages_texts() {
        let chat = [
            Message::User(UserMessage::from("fix it")),
            Message::System(crate::messages::SystemMessage::from("note")),
            Message::Assistant(AssistantMessage::from_text("fixed".to_owned())),
        ];
        assert_eq!(copy_text(&chat, 0..=2), "fix it\n\nfixed");
        assert_eq!(copy_text(&chat, 1..=1), "");
    }
}
