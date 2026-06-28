//! Horizon — a timeblocking nostr calendar app for Notedeck.
//!
//! Time blocks are modelled as NIP-52 calendar events stored in nostrdb —
//! time-based (kind `31923`) and date-based / all-day (kind `31922`) — and
//! rendered in a three-pane calendar: a mini-month + agenda sidebar, a styled
//! day/week timeline, and an inspector for the selected event.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use egui::{Align, RichText, Stroke};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{AppContext, AppResponse};

use block::Block;

mod block;
mod inspector;
mod sidebar;
mod theme;
mod timeline;

/// Which span of time the timeline shows. Only [`View::Day`] and [`View::Week`]
/// have full timelines today; the wider spans are placeholders for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl View {
    const ALL: [View; 5] = [
        View::Day,
        View::Week,
        View::Month,
        View::Quarter,
        View::Year,
    ];

    fn label(self) -> &'static str {
        match self {
            View::Day => "Day",
            View::Week => "Week",
            View::Month => "Month",
            View::Quarter => "Quarter",
            View::Year => "Year",
        }
    }
}

/// Minutes spanned by the keyboard selection cursor and one navigation step
/// (viscal's `timeblock_size`). Resizing it is a later card.
const SELECTION_MINUTES: i64 = 30;

/// Default / min / max pixels per hour for the day grid (viscal's `zoom`).
const HOUR_HEIGHT_DEFAULT: f32 = 56.0;
const HOUR_HEIGHT_MIN: f32 = 22.0;
const HOUR_HEIGHT_MAX: f32 = 220.0;
/// Multiplicative step for one `zi`/`zo` zoom command (viscal's `zoom_amt`).
const ZOOM_STEP: f32 = 1.15;

pub struct Horizon {
    view: View,
    /// The date the timeline is focused on.
    focus: DateTime<Local>,
    /// First-of-month date shown by the sidebar's mini calendar.
    cal_month: NaiveDate,
    /// Index into [`Self::blocks`] of the inspected event, if any.
    selected: Option<usize>,
    /// Keyboard "current time" cursor (viscal's `current`). When nothing is
    /// selected, the selection block sits at `[cursor, cursor + SELECTION)`.
    cursor: DateTime<Local>,
    /// Pending vim-style repeat count for the next navigation command.
    repeat: u32,
    /// Pending first key of a two-key chord (viscal's `cal->chord`): one of
    /// `z`/`g`/`a`, waiting for the second key to complete the command.
    chord: Option<egui::Key>,
    /// Pixels per hour on the day grid; changed by the `zi`/`zo` zoom chords.
    hour_height: f32,
    /// Set for one frame after a keyboard move to scroll the cursor to this
    /// edge of the viewport (`zz` centers, `zt` tops, `zb` bottoms).
    scroll_to_cursor: Option<Align>,
    /// Vertical points to nudge the day scroll by this frame (Ctrl-d/Ctrl-u).
    scroll_delta: f32,
    /// Current search query (not yet wired to filtering).
    search: String,
    /// Time blocks materialized from NIP-52 calendar events.
    blocks: Vec<Block>,
    /// nostrdb subscription for live calendar-event updates.
    sub: Option<Subscription>,
}

impl Default for Horizon {
    fn default() -> Self {
        let now = Local::now();
        Self {
            view: View::Day,
            focus: now,
            cal_month: sidebar::month_of(now),
            selected: None,
            cursor: snap_to_step(now),
            repeat: 1,
            chord: None,
            hour_height: HOUR_HEIGHT_DEFAULT,
            scroll_to_cursor: None,
            scroll_delta: 0.0,
            search: String::new(),
            blocks: Vec::new(),
            sub: None,
        }
    }
}

impl notedeck::App for Horizon {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        // Subscribe once, then backfill the calendar events already in the db —
        // a subscription only reports notes indexed *after* it's created.
        if self.sub.is_none() {
            match ctx.ndb.subscribe(&block::calendar_filters()) {
                Ok(sub) => {
                    self.sub = Some(sub);
                    self.reload(ctx.ndb);
                }
                Err(err) => tracing::error!("horizon: failed to subscribe: {err}"),
            }
        }

        // Re-read whenever new calendar notes have been indexed.
        if let Some(sub) = self.sub
            && !ctx.ndb.poll_for_notes(sub, 256).is_empty()
        {
            self.reload(ctx.ndb);
        }
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.show(ui);
        AppResponse::none()
    }
}

impl Horizon {
    /// Re-read all calendar events from nostrdb into [`Self::blocks`].
    fn reload(&mut self, ndb: &Ndb) {
        let txn = match Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("horizon: txn failed: {err}");
                return;
            }
        };

        let filters = block::calendar_filters();
        let results = ndb.query(&txn, &filters, 5000).unwrap_or_default();

        self.blocks = results
            .iter()
            .filter_map(|r| ndb.get_note_by_key(&txn, r.note_key).ok())
            .filter_map(|note| block::from_note(&note))
            .collect();
        self.blocks.sort_by_key(|b| b.start);
        // A reload can invalidate the old index, so clear the selection.
        self.selected = None;
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        apply_theme(ui);

        // Keyboard navigation drives the day view's cursor/selection. Reset the
        // per-frame scroll signals, then let `handle_keys` raise them again.
        self.scroll_to_cursor = None;
        self.scroll_delta = 0.0;
        if self.view == View::Day {
            self.handle_keys(ui);
        }

        egui::TopBottomPanel::top("horizon_toolbar")
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 8)))
            .show_inside(ui, |ui| self.toolbar(ui));

        egui::SidePanel::left("horizon_sidebar")
            .resizable(true)
            .default_width(300.0)
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 4)))
            .show_inside(ui, |ui| {
                let action =
                    sidebar::show(ui, self.focus, self.cal_month, &self.blocks, self.selected);
                self.apply_sidebar(action);
            });

        egui::SidePanel::right("horizon_inspector")
            .resizable(true)
            .default_width(320.0)
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(16, 4)))
            .show_inside(ui, |ui| {
                inspector::show(ui, &self.blocks, self.selected);
            });

        egui::CentralPanel::default()
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(8, 4)))
            .show_inside(ui, |ui| self.center(ui));
    }

    fn center(&mut self, ui: &mut egui::Ui) {
        match self.view {
            View::Day => {
                let clicked = timeline::center_day(
                    ui,
                    &timeline::DayView {
                        focus: self.focus,
                        blocks: &self.blocks,
                        selected: self.selected,
                        cursor: self.cursor,
                        hour_height: self.hour_height,
                        scroll_to_cursor: self.scroll_to_cursor,
                        scroll_delta: self.scroll_delta,
                    },
                );
                if let Some(i) = clicked {
                    // A mouse click selects a block and snaps the cursor to it.
                    self.selected = Some(i);
                    self.cursor = self.blocks[i].start;
                }
            }
            View::Week => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| timeline::week(ui, self.focus, &self.blocks));
            }
            other => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(
                        RichText::new(format!("{} view coming soon", other.label()))
                            .size(16.0)
                            .color(theme::TEXT_WEAK),
                    );
                });
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if nav_button(ui, "‹") {
                self.shift(-1);
            }
            if ui
                .add(egui::Button::new(RichText::new("Today").color(theme::TEXT)))
                .clicked()
            {
                self.go_today();
            }
            if nav_button(ui, "›") {
                self.shift(1);
            }

            // Red "today" day-of-month badge.
            let today = Local::now().day();
            let (badge, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().circle_filled(badge.center(), 11.0, theme::NOW);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                today.to_string(),
                egui::FontId::proportional(11.0),
                theme::TEXT,
            );

            ui.add_space(16.0);

            // View segmented control.
            for v in View::ALL {
                if tab(ui, v.label(), self.view == v) {
                    self.view = v;
                }
            }

            // Pending keyboard command (repeat count + chord prefix), à la
            // vim's showcmd.
            let pending = self.pending_cmd();
            if !pending.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new(pending).monospace().color(theme::ACCENT_BLUE));
            }

            // Search at the far right.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search")
                        .desired_width(200.0),
                );
            });
        });
    }

    fn apply_sidebar(&mut self, action: sidebar::SidebarAction) {
        if action.month_step != 0 {
            self.cal_month = sidebar::step_month(self.cal_month, action.month_step);
        }
        if let Some(date) = action.focus {
            self.focus = at_local_midnight(date).unwrap_or(self.focus);
            self.cal_month = date.with_day(1).unwrap_or(self.cal_month);
            // Re-home the cursor on the newly focused day and drop any
            // now-off-day selection; a clicked agenda row re-selects below.
            self.cursor = with_date(self.cursor, date);
            self.selected = None;
        }
        if let Some(i) = action.selected {
            self.selected = Some(i);
            self.cursor = self.blocks[i].start;
        }
    }

    fn go_today(&mut self) {
        self.focus = Local::now();
        self.cal_month = sidebar::month_of(self.focus);
        self.cursor = with_date(self.cursor, self.focus.date_naive());
        self.selected = None;
    }

    /// Move the focused range forward/backward by one unit of the current view.
    fn shift(&mut self, units: i64) {
        let days = match self.view {
            View::Week => units * 7,
            _ => units,
        };
        self.focus += Duration::days(days);
        self.cal_month = sidebar::month_of(self.focus);
        self.cursor = with_date(self.cursor, self.focus.date_naive());
        self.selected = None;
    }

    /// Vim-style keyboard navigation for the day view: single-key motions
    /// (`j`/`k`/`t`, repeat digits) plus two-key chords (`zz`/`zt`/`zb` view
    /// positioning, `zi`/`zo` zoom, `gj`/`gk` event hopping, `ah` align to the
    /// hour) and Ctrl-d/Ctrl-u scrolling. No-op while a text field (e.g. the
    /// search box) holds keyboard focus. Mirrors viscal's `on_keypress`.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        use egui::Key;

        if ui.memory(|m| m.focused().is_some()) {
            self.chord = None;
            return;
        }

        // Collect this frame's key presses (with modifiers) in order, plus any
        // ctrl-scroll / pinch zoom gesture.
        let (presses, zoom_delta) = ui.input(|i| {
            let presses: Vec<(Key, egui::Modifiers)> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect();
            (presses, i.zoom_delta())
        });

        // Ctrl-scroll / trackpad pinch zooms the grid (viscal's `on_scroll`).
        if zoom_delta != 1.0 {
            self.set_hour_height(self.hour_height * zoom_delta);
        }

        for (key, mods) in presses {
            // A pending chord consumes the next key; an unmatched second key
            // falls through to normal handling (as in viscal). `take()` always
            // clears the pending chord, even when the pair doesn't match.
            if let Some(first) = self.chord.take()
                && self.run_chord(first, key)
            {
                self.repeat = 1;
                continue;
            }

            // Digit 2–9 sets the repeat count for the next command.
            if let Some(n) = digit_value(key) {
                self.repeat = n;
                continue;
            }

            // Ctrl-d / Ctrl-u scroll the grid by an hour without moving the
            // cursor (viscal's Ctrl-d/Ctrl-u).
            if mods.ctrl {
                match key {
                    Key::D => self.scroll_delta += self.hour_height,
                    Key::U => self.scroll_delta -= self.hour_height,
                    _ => {}
                }
                continue;
            }

            match key {
                Key::J | Key::ArrowDown => self.repeat_move(1),
                Key::K | Key::ArrowUp => self.repeat_move(-1),
                Key::T => {
                    self.cursor_now();
                    self.scroll_to_cursor = Some(Align::Center);
                    self.repeat = 1;
                }
                // Chord prefixes wait for a second key.
                Key::Z | Key::G | Key::A => self.chord = Some(key),
                _ => {}
            }
        }
    }

    /// Run `move_cursor` `repeat` times then center the cursor, resetting the
    /// repeat count.
    fn repeat_move(&mut self, rel: i64) {
        for _ in 0..self.repeat.max(1) {
            self.move_cursor(rel);
        }
        self.repeat = 1;
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// Complete a two-key chord. Returns `false` if `(first, second)` isn't a
    /// known chord, so the caller can handle `second` as a fresh key.
    fn run_chord(&mut self, first: egui::Key, second: egui::Key) -> bool {
        use egui::Key::*;
        let n = self.repeat.max(1) as i32;
        match (first, second) {
            (Z, Z) => self.scroll_to_cursor = Some(Align::Center),
            (Z, T) => self.scroll_to_cursor = Some(Align::Min),
            (Z, B) => self.scroll_to_cursor = Some(Align::Max),
            (Z, I) => self.zoom(n),
            (Z, O) => self.zoom(-n),
            (G, J) => self.select_event(1, n),
            (G, K) => self.select_event(-1, n),
            (A, H) => self.align_hour(),
            // `dd` (delete) is a write op handled by the create/edit/delete card.
            _ => return false,
        }
        true
    }

    /// Zoom the grid in (`steps > 0`) or out, keeping the cursor centered.
    fn zoom(&mut self, steps: i32) {
        self.set_hour_height(self.hour_height * ZOOM_STEP.powi(steps));
        self.scroll_to_cursor = Some(Align::Center);
    }

    fn set_hour_height(&mut self, h: f32) {
        self.hour_height = h.clamp(HOUR_HEIGHT_MIN, HOUR_HEIGHT_MAX);
    }

    /// Hop the selection to the `rel`th next/previous event (viscal's
    /// `select_dir`), following it onto another day if needed.
    fn select_event(&mut self, rel: i64, repeat: i32) {
        if self.blocks.is_empty() {
            return;
        }
        for _ in 0..repeat.max(1) {
            let next = match self.selected {
                Some(i) => (i as i64 + rel).clamp(0, self.blocks.len() as i64 - 1) as usize,
                None => match self.closest_event(rel) {
                    Some(i) => i,
                    None => return,
                },
            };
            self.selected = Some(next);
            self.cursor = self.blocks[next].start;
        }
        // Follow the selection onto its day and bring it into view.
        self.focus = with_date(self.focus, self.cursor.date_naive());
        self.cal_month = sidebar::month_of(self.focus);
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// Index of the nearest event in direction `rel` relative to the cursor
    /// (viscal's `find_closest_event`).
    fn closest_event(&self, rel: i64) -> Option<usize> {
        if rel > 0 {
            self.blocks.iter().position(|b| b.start > self.cursor)
        } else {
            self.blocks.iter().rposition(|b| b.start < self.cursor)
        }
    }

    /// The pending vim-style command shown in the toolbar (repeat count and/or
    /// chord prefix), empty when nothing is pending.
    fn pending_cmd(&self) -> String {
        use egui::Key;
        let mut s = String::new();
        if self.repeat != 1 {
            s.push_str(&self.repeat.to_string());
        }
        s.push_str(match self.chord {
            Some(Key::Z) => "z",
            Some(Key::G) => "g",
            Some(Key::A) => "a",
            _ => "",
        });
        s
    }

    /// Snap the cursor to the nearest hour, deselecting (viscal's `align_hour`).
    fn align_hour(&mut self) {
        let minute = self.cursor.minute();
        let floored = self.cursor
            - Duration::minutes(minute as i64)
            - Duration::seconds(self.cursor.second() as i64);
        self.cursor = if minute >= 30 {
            floored + Duration::hours(1)
        } else {
            floored
        };
        self.selected = None;
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// Move the cursor one step in `rel` (±1), snapping onto an overlapping
    /// timed event if the new selection window hits one — viscal's
    /// `move_relative` (viscal.c:1050).
    fn move_cursor(&mut self, rel: i64) {
        let step = Duration::minutes(SELECTION_MINUTES);

        // Base the move on the selected event's edges, else the bare cursor.
        self.cursor = match self.selected.and_then(|i| self.blocks.get(i)) {
            Some(b) if !b.all_day => {
                if rel > 0 {
                    b.end
                } else {
                    b.start - step
                }
            }
            _ => self.cursor + step * rel as i32,
        };

        // Snap onto the first timed event overlapping the new window.
        match self.block_overlapping(self.cursor, self.cursor + step) {
            Some(i) => {
                self.cursor = self.blocks[i].start;
                self.selected = Some(i);
            }
            None => self.selected = None,
        }
    }

    /// Jump the cursor (and focus) to "now", deselecting any event.
    fn cursor_now(&mut self) {
        let now = Local::now();
        self.focus = now;
        self.cal_month = sidebar::month_of(now);
        self.cursor = snap_to_step(now);
        self.selected = None;
    }

    /// First timed (non-all-day) block whose span overlaps `[start, end)`.
    fn block_overlapping(&self, start: DateTime<Local>, end: DateTime<Local>) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| !b.all_day && b.start < end && start < b.end)
    }
}

/// Map a digit key 2–9 to its repeat-count value (1 is the implicit default,
/// so it's excluded), matching viscal's repeat handling.
fn digit_value(key: egui::Key) -> Option<u32> {
    use egui::Key;
    match key {
        Key::Num2 => Some(2),
        Key::Num3 => Some(3),
        Key::Num4 => Some(4),
        Key::Num5 => Some(5),
        Key::Num6 => Some(6),
        Key::Num7 => Some(7),
        Key::Num8 => Some(8),
        Key::Num9 => Some(9),
        _ => None,
    }
}

/// Push Horizon's dark palette into the ambient egui style for this frame.
fn apply_theme(ui: &mut egui::Ui) {
    let v = &mut ui.style_mut().visuals;
    v.panel_fill = theme::BG;
    v.window_fill = theme::BG;
    v.extreme_bg_color = theme::SURFACE;
    v.override_text_color = Some(theme::TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme::GRID);
    v.widgets.inactive.bg_fill = theme::SURFACE;
    v.widgets.hovered.bg_fill = theme::GRID;
}

fn panel_frame() -> egui::Frame {
    egui::Frame::new().fill(theme::BG)
}

fn nav_button(ui: &mut egui::Ui, glyph: &str) -> bool {
    ui.add(egui::Button::new(
        RichText::new(glyph).size(18.0).color(theme::TEXT_WEAK),
    ))
    .clicked()
}

/// A segmented-control tab; highlighted when `active`.
fn tab(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let color = if active {
        theme::TEXT
    } else {
        theme::TEXT_WEAK
    };
    let mut btn = egui::Button::new(RichText::new(label).color(color)).frame(false);
    if active {
        btn = btn.fill(theme::SURFACE);
    }
    ui.add(btn).clicked()
}

/// The local midnight that starts the (Monday-based) week containing `dt`.
pub(crate) fn start_of_week(dt: DateTime<Local>) -> DateTime<Local> {
    let back = dt.weekday().num_days_from_monday() as i64;
    let monday = dt.date_naive() - chrono::Duration::days(back);
    at_local_midnight(monday).unwrap_or(dt)
}

/// Resolve a `NaiveDate` to local midnight, handling DST gaps gracefully.
fn at_local_midnight(date: NaiveDate) -> Option<DateTime<Local>> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()
}

/// Round a timestamp to the nearest 5-minute boundary (viscal's
/// `SMALLEST_TIMEBLOCK`), used to land the cursor on a tidy minute.
fn snap_to_step(dt: DateTime<Local>) -> DateTime<Local> {
    const STEP: i64 = 5 * 60;
    let snapped = (dt.timestamp() + STEP / 2).div_euclid(STEP) * STEP;
    Local.timestamp_opt(snapped, 0).single().unwrap_or(dt)
}

/// Move `dt` onto `date`, keeping its local time-of-day (used to re-home the
/// cursor when the focused day changes).
fn with_date(dt: DateTime<Local>, date: NaiveDate) -> DateTime<Local> {
    Local
        .from_local_datetime(&date.and_time(dt.time()))
        .single()
        .unwrap_or(dt)
}

/// Fraction (0.0..1.0) of the way through the day that `dt`'s local time is.
pub(crate) fn day_fraction(dt: DateTime<Local>) -> f32 {
    dt.num_seconds_from_midnight() as f32 / 86_400.0
}
