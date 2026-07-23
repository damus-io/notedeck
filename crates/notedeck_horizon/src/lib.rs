//! Horizon — a timeblocking nostr calendar app for Notedeck.
//!
//! Time blocks are modelled as NIP-52 calendar events stored in nostrdb —
//! time-based (kind `31923`) and date-based / all-day (kind `31922`) — and
//! rendered in a three-pane calendar: a mini-month + agenda sidebar, a styled
//! day/week timeline, and an inspector for the selected event.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use egui::{Align, RichText, Stroke};
use enostr::{Pubkey, RelayId};
use nostrdb::{Filter, IngestMetadata, Ndb, Note, NoteBuilder, Subscription, Transaction};
use notedeck::{AppContext, AppResponse, PrivateRelaySync, fan_out_event_frame};

use block::Block;

mod block;
mod inspector;
mod month;
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

/// A block's `[start, end)` span, used as scratch state while planning a
/// push-move cascade before any of it is written back (see [`Horizon::spans`]).
#[derive(Clone, Copy)]
struct Span {
    start: DateTime<Local>,
    end: DateTime<Local>,
}

impl Span {
    fn duration(&self) -> Duration {
        self.end - self.start
    }
}

/// One planned cascade move: block `index` relocated to `[start, end)`. A full
/// plan is a list of these, applied to the scratch [`Span`]s and re-published.
struct SpanMove {
    index: usize,
    start: DateTime<Local>,
    end: DateTime<Local>,
}

/// An in-progress inline title edit of the selected block (viscal's edit buffer
/// under `CAL_CHANGING`). Rendered as a `TextEdit` overlaid on the block; `c`/`s`
/// start it cleared, `A` starts it from the current title, insert (`i`/`o`)
/// starts it fresh.
pub(crate) struct EditState {
    /// The working title being typed.
    buf: String,
    /// The value to restore into `buf` until the field first grabs focus, so the
    /// keystroke that opened the editor can't leak in as the first character.
    initial: String,
    /// Whether the field has taken keyboard focus yet (see `initial`).
    focused: bool,
    /// True when editing a just-inserted event, so cancelling deletes it
    /// (viscal's `CAL_INSERTING`); a plain rename leaves the event in place.
    fresh: bool,
}

/// How an inline edit ended this frame (the `TextEdit` lost focus).
pub(crate) enum EditOutcome {
    /// Return / click-away: keep what was typed.
    Commit,
    /// Escape: discard; delete the event too if it was a fresh insert.
    Cancel,
}

/// Day columns the week view shows: the full week normally, but only a narrow
/// span on phones, where seven columns are unreadable at ~390px (the epic asks
/// for "~3 day columns, not 7").
const WEEK_DAYS: usize = 7;
const WEEK_DAYS_NARROW: usize = 3;

/// Day cells in the month grid: six Sunday-first weeks, enough to cover any
/// month's layout without the number of rows jumping around.
const MONTH_CELLS: usize = 42;

/// Minutes spanned by the keyboard selection cursor and one navigation step
/// (viscal's `timeblock_size`). Resizing it is a later card.
const SELECTION_MINUTES: i64 = 30;

/// The finest grain a block moves or resizes by, per `repeat` (viscal's
/// `SMALLEST_TIMEBLOCK`). Coarser than [`SELECTION_MINUTES`] navigation on
/// purpose: nudging an event should feel precise.
const MOVE_MINUTES: i64 = 5;

/// Minimum Horizon-content width (points) for the persistent inspector side
/// pane. Below it the three-pane layout squeezes the timeline unusably — a
/// ~300px sidebar plus a ~320px inspector eat 620px of chrome — so the
/// inspector is dropped and the selected event opens as a separate detail view
/// instead. Measured from `ui.available_width()` (Horizon's real allotment,
/// minus the chrome nav rail), not the screen: there's no notedeck helper for
/// this tier, since `is_narrow` (550px) only covers the single-column phone
/// case, so it's a Horizon-local breakpoint.
const INSPECTOR_MIN_WIDTH: f32 = 1100.0;

/// Default / min / max pixels per hour for the day grid (viscal's `zoom`).
const HOUR_HEIGHT_DEFAULT: f32 = 56.0;
const HOUR_HEIGHT_MIN: f32 = 22.0;
const HOUR_HEIGHT_MAX: f32 = 220.0;
/// Multiplicative step for one `zi`/`zo` zoom command (viscal's `zoom_amt`).
const ZOOM_STEP: f32 = 1.15;

/// The inputs the cached [`timeline::DayLayout`]s were built for. A change in
/// any of them (the blocks moved, or a different date range scrolled into view)
/// invalidates the cache; an unchanged key lets a frame reuse it as-is.
#[derive(Clone, Copy, PartialEq)]
struct LayoutKey {
    /// Generation counter, bumped whenever [`Horizon::blocks`] changes.
    generation: u64,
    /// First visible date (the focused day, or the week's Monday).
    first: NaiveDate,
    /// Number of consecutive days laid out (1 for the day view, 7 for the week).
    days: usize,
}

/// Cached per-day block layouts, so the timeline draw never re-runs
/// [`block::layout`] or re-collects per-day index lists on the per-frame path.
/// Rebuilt by [`Horizon::ensure_layout`] only when its [`LayoutKey`] changes.
#[derive(Default)]
struct LayoutCache {
    key: Option<LayoutKey>,
    days: Vec<timeline::DayLayout>,
}

pub struct Horizon {
    view: View,
    /// The date the timeline is focused on.
    focus: DateTime<Local>,
    /// Reference "now" for this frame — the wall clock, refreshed each render,
    /// or a pinned instant when [`Self::now_override`] is set. Centralizing it
    /// keeps the now-line, the "today" badge and date highlighting in agreement
    /// instead of each reading `Local::now()` at a slightly different moment.
    now: DateTime<Local>,
    /// When set, pins [`Self::now`] instead of reading the wall clock — a
    /// test-only seam (see [`Horizon::pin_now`]) for deterministic snapshots.
    now_override: Option<DateTime<Local>>,
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
    /// Event id of a just-created block to re-select after the next reload,
    /// once the async ingest round-trips it back out of nostrdb.
    pending_select: Option<[u8; 32]>,
    /// Event ids we've published a NIP-09 deletion for this session. nostrdb
    /// doesn't yet drop deleted notes, so we filter them out of every [`reload`]
    /// ourselves to keep an `x`/`dd` delete from resurrecting on the next poll.
    deleted: HashSet<[u8; 32]>,
    /// `d`-tags (block UIDs) locked against push-move (viscal's `EV_IMMOVABLE`,
    /// toggled with `l`). A locked block is an anchor: a push/expand cascade that
    /// would have to displace it is refused. Keyed on the stable UID so the flag
    /// survives the re-publish that a move/edit mints. Session-local for now.
    locked: HashSet<String>,
    /// In-progress inline title edit of the selected block, if any (viscal's
    /// `CAL_CHANGING`). While set, a `TextEdit` overlays the block and holds
    /// keyboard focus, so the normal nav keys are suppressed.
    editing: Option<EditState>,
    /// Cross-device sync of the account's own calendar events over its private
    /// relays (inbound subscription); resolves [`Self::publish_relays`].
    private_sync: PrivateRelaySync,
    /// Private relays resolved by [`Self::private_sync`] this frame, reused as
    /// the outbound publish targets for events we create. Empty => local-only.
    publish_relays: Vec<RelayId>,
    /// Background worker mirroring the platform calendar (e.g. macOS EventKit)
    /// into nostrdb. `None` until started; stays `None` on platforms without a
    /// native calendar source. Dropping it stops the worker.
    calsync: Option<nostr_calsync::SyncHandle>,
    /// Whether we've already tried to start [`Self::calsync`], so we attempt it
    /// at most once per run rather than every frame.
    calsync_started: bool,
    /// Bumped every time [`Self::blocks`] changes, to invalidate [`Self::layout`].
    layout_gen: u64,
    /// Cached per-day block layouts, rebuilt only when the blocks or the visible
    /// date range change — see [`Self::ensure_layout`].
    layout: LayoutCache,
    /// Whether the fullscreen event-detail view is open. Only meaningful below
    /// desktop width, where there's no inspector pane: tapping an event opens
    /// it over the timeline, its nav bar's back affordance (or Escape) closes
    /// it, and prev/next hop events in the `gj`/`gk` order. Forced back to
    /// `false` on any frame it can't apply (widened to show the pane, or the
    /// selection cleared), so it never dangles.
    detail: bool,
}

impl Default for Horizon {
    fn default() -> Self {
        let now = Local::now();
        Self {
            view: View::Day,
            focus: now,
            now,
            now_override: None,
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
            pending_select: None,
            deleted: HashSet::new(),
            locked: HashSet::new(),
            editing: None,
            private_sync: PrivateRelaySync::new("horizon"),
            publish_relays: Vec::new(),
            calsync: None,
            calsync_started: false,
            layout_gen: 0,
            layout: LayoutCache::default(),
            detail: false,
        }
    }
}

impl notedeck::App for Horizon {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        // Sync this account's own calendar events across its devices over the
        // private relays, and remember the resolved set as our publish targets.
        let author = *ctx.accounts.selected_account_pubkey();
        self.publish_relays = self.private_sync.update(ctx, calendar_sync_filter(&author));

        // Start mirroring the platform calendar into nostrdb once we have an
        // account to sign the imported events with. The worker writes the same
        // NIP-52 notes Horizon already reads, so they flow back in through the
        // subscription below.
        self.start_calsync(ctx);

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

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.show(ctx, ui);
        AppResponse::none()
    }
}

impl Horizon {
    /// Start the calendar-mirror worker, at most once, as soon as a signing key
    /// is available. Imported events are attributed to the selected account.
    fn start_calsync(&mut self, ctx: &mut AppContext<'_>) {
        if self.calsync_started {
            return;
        }

        // Need a secret key to sign the imported notes; wait until one's around.
        let Some(keys) = ctx.accounts.selected_filled() else {
            return;
        };
        let secret = keys.secret_key.secret_bytes();

        // Only one attempt: a real source prompts for calendar access, and on
        // platforms without one there's nothing to start.
        self.calsync_started = true;

        // Opt-in for now. Requesting calendar access needs an Info.plist usage
        // string, which only the bundled `.app` carries — an unbundled
        // `cargo run` would be killed by macOS TCC the moment it asks. Gate on an
        // env var until calendar access is exposed as a proper setting.
        if std::env::var_os("NOTEDECK_CALSYNC").is_none() {
            return;
        }

        self.calsync = nostr_calsync::spawn_default(
            ctx.ndb.clone(),
            secret,
            nostr_calsync::SyncConfig::default(),
        );
    }

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
            .filter(|b| !self.deleted.contains(&b.id))
            .collect();
        self.blocks.sort_by_key(|b| b.start);
        self.invalidate_layout();
        // A reload can invalidate the old index. Re-find a just-created event by
        // its id to keep it selected; otherwise clear the now-stale selection.
        self.selected = self
            .pending_select
            .take()
            .and_then(|id| self.blocks.iter().position(|b| b.id == id));
    }

    /// Mark the cached [`Self::layout`] stale after a change to [`Self::blocks`].
    /// Every path that adds, removes, or re-times a block calls this so the next
    /// frame rebuilds the affected day layouts (title-only edits don't, as they
    /// can't change lane assignment).
    fn invalidate_layout(&mut self) {
        self.layout_gen = self.layout_gen.wrapping_add(1);
    }

    /// Rebuild [`Self::layout`] for the currently-visible date(s) if the blocks
    /// or the visible range changed since it was last built. A no-op when the
    /// [`LayoutKey`] is unchanged, so the common frame does no work; when the
    /// view shows no timeline (month/agenda) the cache is simply emptied. Must
    /// run before the timeline reads `self.layout`. `narrow` picks the phone
    /// week span (a 3-day window from the focused day) over the full week.
    fn ensure_layout(&mut self, narrow: bool) {
        let (first, days) = match self.view {
            View::Day => (self.focus.date_naive(), 1),
            // The full week starts on its Monday; the narrow phone window runs
            // from the focused day so ‹ / › page through three days at a time.
            View::Week if narrow => (self.focus.date_naive(), WEEK_DAYS_NARROW),
            View::Week => (start_of_week(self.focus).date_naive(), WEEK_DAYS),
            // The month grid is six Sunday-first weeks: start on the Sunday on
            // or before the 1st, then lay out all 42 cells.
            View::Month => (month_grid_start(self.focus), MONTH_CELLS),
            _ => {
                self.layout.days.clear();
                self.layout.key = None;
                return;
            }
        };

        let key = LayoutKey {
            generation: self.layout_gen,
            first,
            days,
        };
        if self.layout.key == Some(key) {
            return;
        }

        self.layout.days.clear();
        for d in 0..days as i64 {
            let date = first + Duration::days(d);
            self.layout
                .days
                .push(timeline::DayLayout::build(&self.blocks, date));
        }
        self.layout.key = Some(key);
    }

    fn show(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        apply_theme(ui);

        // Resolve this frame's reference clock once (pinned in tests) so the
        // now-line, the "today" badge and any "jump to now" keys agree on it.
        self.now = self.now_override.unwrap_or_else(Local::now);

        let narrow = notedeck::ui::is_narrow(ui.ctx());
        // Three width tiers, from Horizon's own allotment (see
        // [`INSPECTOR_MIN_WIDTH`]): below `is_narrow` (550) a single column;
        // between that and `wide` the sidebar + timeline but no inspector; at
        // `wide` and up the full three panes.
        let wide = ui.available_width() >= INSPECTOR_MIN_WIDTH;

        // Below desktop width the selected event opens as a fullscreen detail
        // view over the timeline (there's no inspector pane to hold it). Keep
        // the flag honest: it can't apply once the pane is back or nothing is
        // selected.
        let show_detail = self.detail && !wide && self.selected.is_some();
        self.detail = show_detail;

        // Keyboard navigation drives the day view's cursor/selection. Reset the
        // per-frame scroll signals, then let the handlers raise them again. The
        // detail view has its own keys (back / prev / next); otherwise the day
        // view takes them.
        self.scroll_to_cursor = None;
        self.scroll_delta = 0.0;
        if show_detail {
            self.handle_detail_keys(ui);
        } else if self.view == View::Day {
            self.handle_keys(ctx, ui);
        } else {
            // Only the day view renders the inline editor; drop any pending edit
            // so it can't dangle unresolved after a view switch.
            self.editing = None;
        }

        egui::TopBottomPanel::top("horizon_toolbar")
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 8)))
            .show_inside(ui, |ui| self.toolbar(ui, narrow));

        // On phones the sidebar + inspector leave the timeline no usable room,
        // so collapse to a single column and move the view switcher into a
        // bottom bar. The tap-to-open detail sheet is a later card.
        if narrow {
            egui::TopBottomPanel::bottom("horizon_mobile_bar")
                .frame(panel_frame().inner_margin(egui::Margin::symmetric(8, 8)))
                .show_inside(ui, |ui| self.mobile_view_bar(ui));
        } else {
            let today = self.now.date_naive();
            egui::SidePanel::left("horizon_sidebar")
                .resizable(true)
                .default_width(300.0)
                .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 4)))
                .show_inside(ui, |ui| {
                    let action = sidebar::show(
                        ui,
                        self.focus,
                        today,
                        self.cal_month,
                        &self.blocks,
                        self.selected,
                    );
                    // Tapping an agenda row below desktop width opens the event
                    // in the fullscreen detail view (no inspector pane here).
                    let picked = action.selected.is_some();
                    self.apply_sidebar(action);
                    if picked && !wide {
                        self.detail = true;
                    }
                });

            // The inspector is a persistent pane only when there's room for it
            // beside the sidebar and timeline; on tablet / phone-landscape it
            // would squeeze the timeline, so it's dropped (the selected event
            // opens as its own detail view — see hole-grape-artist).
            if wide {
                let selected_locked = self.selected.is_some_and(|i| self.is_locked(i));
                egui::SidePanel::right("horizon_inspector")
                    .resizable(true)
                    .default_width(320.0)
                    .frame(panel_frame().inner_margin(egui::Margin::symmetric(16, 4)))
                    .show_inside(ui, |ui| {
                        inspector::show(ui, &self.blocks, self.selected, selected_locked);
                    });
            }
        }

        egui::CentralPanel::default()
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(8, 4)))
            .show_inside(ui, |ui| {
                if show_detail {
                    self.event_detail(ui);
                } else {
                    self.center(ctx, ui, wide, narrow);
                }
            });
    }

    /// The fullscreen event-detail screen (below desktop width): a nav bar over
    /// the same fields the inspector pane shows. Back closes it; prev/next hop
    /// to the neighbouring event in `gj`/`gk` order, keeping it open.
    fn event_detail(&mut self, ui: &mut egui::Ui) {
        let locked = self.selected.is_some_and(|i| self.is_locked(i));
        match inspector::show_fullscreen(ui, &self.blocks, self.selected, locked) {
            inspector::DetailAction::Back => self.detail = false,
            inspector::DetailAction::Prev => self.select_event(-1, 1),
            inspector::DetailAction::Next => self.select_event(1, 1),
            inspector::DetailAction::None => {}
        }
    }

    fn center(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui, wide: bool, narrow: bool) {
        // Refresh the cached per-day layouts before any borrow of them below.
        self.ensure_layout(narrow);
        match self.view {
            View::Day => {
                // Split borrows: `DayView` reads `self.blocks`/`self.layout`, the
                // editor takes `&mut self.editing` — disjoint fields, so they
                // coexist. `ensure_layout` guarantees `days[0]` is the focused day.
                let response = {
                    let dv = timeline::DayView {
                        focus: self.focus,
                        now: self.now,
                        blocks: &self.blocks,
                        layout: &self.layout.days[0],
                        selected: self.selected,
                        cursor: self.cursor,
                        hour_height: self.hour_height,
                        scroll_to_cursor: self.scroll_to_cursor,
                        scroll_delta: self.scroll_delta,
                    };
                    timeline::center_day(ui, &dv, self.editing.as_mut())
                };

                // Settle a finished inline edit before a click moves the
                // selection, so a click-away commits the block being edited.
                match response.edit {
                    Some(EditOutcome::Commit) => self.commit_edit(ctx),
                    Some(EditOutcome::Cancel) => self.cancel_edit(ctx),
                    None => {}
                }
                if let Some(i) = response.clicked {
                    // A mouse click selects a block and snaps the cursor to it.
                    self.selected = Some(i);
                    self.cursor = self.blocks[i].start;
                    // Below desktop width there's no inspector pane, so surface
                    // the tapped event in the fullscreen detail view instead.
                    if !wide {
                        self.detail = true;
                    }
                }
            }
            View::Week => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        timeline::week(ui, self.now, &self.blocks, &self.layout.days)
                    });
            }
            View::Month => {
                if let Some(date) = month::show(
                    ui,
                    self.focus,
                    self.now,
                    &self.blocks,
                    &self.layout.days,
                    narrow,
                ) {
                    // Tapping a day cell drops into that day's timeline.
                    self.focus = at_local_midnight(date).unwrap_or(self.focus);
                    self.cal_month = date.with_day(1).unwrap_or(self.cal_month);
                    self.cursor = with_date(self.cursor, date);
                    self.selected = None;
                    self.view = View::Day;
                }
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

    fn toolbar(&mut self, ui: &mut egui::Ui, narrow: bool) {
        ui.horizontal(|ui| {
            if nav_button(ui, "‹") {
                self.shift(-1, narrow);
            }
            if ui
                .add(egui::Button::new(RichText::new("Today").color(theme::TEXT)))
                .clicked()
            {
                self.go_today();
            }
            if nav_button(ui, "›") {
                self.shift(1, narrow);
            }

            // Red "today" day-of-month badge.
            let today = self.now.day();
            let (badge, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().circle_filled(badge.center(), 11.0, theme::NOW);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                today.to_string(),
                egui::FontId::proportional(11.0),
                theme::TEXT,
            );

            // On phones the view switcher moves to the bottom bar and the search
            // box is dropped (it isn't wired to filtering yet), so the toolbar
            // keeps only the date controls and nothing overflows the width.
            if !narrow {
                ui.add_space(16.0);
                self.view_tabs(ui);
            }

            // Pending keyboard command (repeat count + chord prefix), à la
            // vim's showcmd.
            let pending = self.pending_cmd();
            if !pending.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new(pending).monospace().color(theme::ACCENT_BLUE));
            }

            if !narrow {
                // Search at the far right.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search")
                            .desired_width(200.0),
                    );
                });
            }
        });
    }

    /// The Day/Week/Month/… segmented control, laid out inline (desktop toolbar).
    fn view_tabs(&mut self, ui: &mut egui::Ui) {
        for v in View::ALL {
            if tab(ui, v.label(), self.view == v) {
                self.view = v;
            }
        }
    }

    /// The phone bottom bar: the same view switcher spread evenly across the
    /// width as a tab bar, since it doesn't fit up top on a narrow screen.
    fn mobile_view_bar(&mut self, ui: &mut egui::Ui) {
        let current = self.view;
        let mut chosen = current;
        ui.columns(View::ALL.len(), |cols| {
            for (col, v) in cols.iter_mut().zip(View::ALL) {
                col.vertical_centered(|ui| {
                    if tab(ui, v.label(), current == v) {
                        chosen = v;
                    }
                });
            }
        });
        self.view = chosen;
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
        self.focus = self.now;
        self.cal_month = sidebar::month_of(self.focus);
        self.cursor = with_date(self.cursor, self.focus.date_naive());
        self.selected = None;
    }

    /// Move the focused range forward/backward by one unit of the current view.
    /// The week pages by its visible span — a full week normally, three days on
    /// a narrow phone — and the month by a whole month, so ‹ / › always turn one
    /// screenful.
    fn shift(&mut self, units: i64, narrow: bool) {
        self.focus = match self.view {
            View::Month => {
                let month = sidebar::step_month(sidebar::month_of(self.focus), units as i32);
                with_date(self.focus, month)
            }
            View::Week if narrow => self.focus + Duration::days(units * WEEK_DAYS_NARROW as i64),
            View::Week => self.focus + Duration::days(units * WEEK_DAYS as i64),
            _ => self.focus + Duration::days(units),
        };
        self.cal_month = sidebar::month_of(self.focus);
        self.cursor = with_date(self.cursor, self.focus.date_naive());
        self.selected = None;
    }

    /// Vim-style keyboard navigation for the day view: single-key motions
    /// (`j`/`k`/`t`, repeat digits) plus two-key chords (`zz`/`zt`/`zb` view
    /// positioning, `zi`/`zo` zoom, `gj`/`gk` event hopping, `ah` align to the
    /// hour) and Ctrl-d/Ctrl-u scrolling. No-op while a text field (e.g. the
    /// search box) holds keyboard focus. Mirrors viscal's `on_keypress`.
    fn handle_keys(&mut self, ctx: &mut AppContext<'_>, ui: &egui::Ui) {
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
                && self.run_chord(ctx, first, key)
            {
                self.repeat = 1;
                continue;
            }

            // Digit 2–9 sets the repeat count for the next command.
            if let Some(n) = digit_value(key) {
                self.repeat = n;
                continue;
            }

            // Ctrl-based commands: scroll the grid (Ctrl-d/Ctrl-u) and push-move
            // the selection, shoving neighbours along (Ctrl-j/Ctrl-k/Ctrl-v).
            if mods.ctrl {
                match key {
                    Key::D => self.scroll_delta += self.hour_height,
                    Key::U => self.scroll_delta -= self.hour_height,
                    Key::J => self.pushmove_selected(ctx, 1),
                    Key::K => self.pushmove_selected(ctx, -1),
                    Key::V => self.push_expand_selected(ctx),
                    _ => {}
                }
                continue;
            }

            match key {
                // Shift-J/K move the *selected event* by `repeat` steps; plain
                // j/k (and the arrows) move the cursor (viscal's J/K vs j/k).
                Key::J if mods.shift => self.move_selected(ctx, 1),
                Key::K if mods.shift => self.move_selected(ctx, -1),
                Key::J | Key::ArrowDown => self.repeat_move(1),
                Key::K | Key::ArrowUp => self.repeat_move(-1),
                // `t` jumps the view to now; `T` moves the selected event to now.
                Key::T if mods.shift => self.move_selected_to_now(ctx),
                Key::T => {
                    self.cursor_now();
                    self.scroll_to_cursor = Some(Align::Center);
                    self.repeat = 1;
                }
                // `v` grows the selected event's end; `V` shrinks it.
                Key::V if mods.shift => self.resize_selected(ctx, -1),
                Key::V => self.resize_selected(ctx, 1),
                // `x` deletes the selected event; `l` locks it against push-move.
                Key::X => self.delete_selected(ctx),
                Key::L => self.toggle_lock(),
                // `i` inserts a block at the cursor; `o` opens one below the
                // selection (viscal's insert / open_below).
                Key::I => self.insert_at_cursor(ctx),
                Key::O => self.open_below(ctx),
                // `c`/`s` rename the selected event from a blank buffer; `A`
                // edits its current title (viscal's edit / append).
                Key::C | Key::S => self.begin_edit(true),
                Key::A if mods.shift => self.begin_edit(false),
                // Chord prefixes wait for a second key (`dd` deletes + pulls up).
                Key::Z | Key::G | Key::A | Key::D => self.chord = Some(key),
                _ => {}
            }
        }
    }

    /// Keys for the fullscreen event-detail view: Escape (or Backspace) backs
    /// out to the timeline; `j`/`k` and the down/up arrows hop to the next /
    /// previous event, matching the nav bar's arrows and the `gj`/`gk` order.
    /// No-op while a text field holds focus.
    fn handle_detail_keys(&mut self, ui: &egui::Ui) {
        use egui::Key;

        if ui.memory(|m| m.focused().is_some()) {
            return;
        }

        let presses: Vec<Key> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => Some(*key),
                    _ => None,
                })
                .collect()
        });

        for key in presses {
            match key {
                Key::Escape | Key::Backspace => self.detail = false,
                Key::J | Key::ArrowDown => self.select_event(1, 1),
                Key::K | Key::ArrowUp => self.select_event(-1, 1),
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
    fn run_chord(&mut self, ctx: &mut AppContext<'_>, first: egui::Key, second: egui::Key) -> bool {
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
            // `dd` deletes the selected event and pulls the day's later events up.
            (D, D) => self.delete_timeblock(ctx),
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
            Some(Key::D) => "d",
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
        let now = self.now;
        self.focus = now;
        self.cal_month = sidebar::month_of(now);
        self.cursor = snap_to_step(now);
        self.selected = None;
    }

    /// Pin the reference clock to `at` and re-home the view onto it — a
    /// test-only seam (used by the snapshot suite) so rendering is deterministic
    /// regardless of the wall clock. Production never calls this.
    #[doc(hidden)]
    pub fn pin_now(&mut self, at: DateTime<Local>) {
        self.now_override = Some(at);
        self.now = at;
        self.focus = at;
        self.cal_month = sidebar::month_of(at);
        self.cursor = snap_to_step(at);
        self.selected = None;
    }

    /// Number of calendar blocks currently materialized from nostrdb. A
    /// test-only seam so the snapshot harness can wait out nostrdb's async
    /// ingest before rendering; production never calls it.
    #[doc(hidden)]
    pub fn loaded_block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Switch the active view — a test-only seam so the snapshot suite can
    /// render the week/month layouts (production switches from the view tabs).
    #[doc(hidden)]
    pub fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// Select the first timed block and open the fullscreen event-detail view
    /// on it — a test-only seam so the snapshot suite can exercise the detail
    /// screen deterministically (production opens it from a tap). No-op with no
    /// timed blocks loaded.
    #[doc(hidden)]
    pub fn open_first_timed_detail(&mut self) {
        if let Some(i) = self.blocks.iter().position(|b| !b.all_day) {
            self.selected = Some(i);
            self.cursor = self.blocks[i].start;
            self.detail = true;
        }
    }

    /// `i` — insert a new timed block at the cursor (viscal's `insert_event`).
    fn insert_at_cursor(&mut self, ctx: &mut AppContext<'_>) {
        let start = self.cursor;
        self.create_event(ctx, start, start + Duration::minutes(SELECTION_MINUTES));
    }

    /// `o` — open a new block immediately below the selection, or at the cursor
    /// when nothing is selected (viscal's `open_below`; cascading the neighbours
    /// out of the way is the push-move card).
    fn open_below(&mut self, ctx: &mut AppContext<'_>) {
        let start = match self.selected.and_then(|i| self.blocks.get(i)) {
            Some(b) if !b.all_day => b.end,
            _ => self.cursor,
        };
        self.create_event(ctx, start, start + Duration::minutes(SELECTION_MINUTES));
    }

    /// Build, ingest and publish a kind-31923 time-based event over
    /// `[start, end)`, then optimistically place it on the timeline and select
    /// it so it's ready to edit. No-op (with a log) for a watch-only account.
    fn create_event(
        &mut self,
        ctx: &mut AppContext<'_>,
        start: DateTime<Local>,
        end: DateTime<Local>,
    ) {
        self.repeat = 1;

        // Mutations need a signing key; a watch-only account simply can't write.
        let Some(secret) = account_secret(ctx) else {
            tracing::warn!("horizon: can't create an event with a watch-only account");
            return;
        };

        let title = "New event";
        let d = unique_d_tag(start);
        let builder = NoteBuilder::new()
            .content("")
            .kind(block::KIND_TIME_BASED as u32)
            .start_tag()
            .tag_str("d")
            .tag_str(&d)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("start")
            .tag_str(&start.timestamp().to_string())
            .start_tag()
            .tag_str("end")
            .tag_str(&end.timestamp().to_string());

        let Some(note) = builder.sign(&secret).build() else {
            tracing::error!("horizon: failed to build calendar note");
            return;
        };
        let id = *note.id();

        // Ingest locally and fan out to the account's private relays.
        let Some(json) = frame_note(&note) else {
            return;
        };
        self.publish_json(ctx, json);

        // Show it immediately rather than waiting for the async ingest to round
        // back through the subscription; `pending_select` keeps it selected once
        // the reload replaces this optimistic copy with the stored one.
        self.blocks.push(Block::timed(id, &d, title, start, end));
        self.blocks.sort_by_key(|b| b.start);
        self.invalidate_layout();
        self.cursor = start;
        self.selected = self.blocks.iter().position(|b| b.id == id);
        self.pending_select = Some(id);
        self.scroll_to_cursor = Some(Align::Center);

        // Drop straight into an inline edit so the new block can be named right
        // away; Escape here deletes it again (viscal's insert → CAL_INSERTING).
        self.editing = Some(EditState {
            buf: String::new(),
            initial: String::new(),
            focused: false,
            fresh: true,
        });
    }

    /// Ingest a pre-framed `["EVENT", …]` json into the local nostrdb, then fan
    /// the same frame out to the account's private relays so it syncs across the
    /// account's devices. Shared by every create/edit/delete write path.
    fn publish_json(&mut self, ctx: &mut AppContext<'_>, json: String) {
        if let Err(err) = ctx
            .ndb
            .process_event_with(&json, IngestMetadata::new().client(true))
        {
            tracing::error!("horizon: failed to ingest calendar note: {err}");
            return;
        }
        fan_out_event_frame(
            &mut ctx.remote.publisher_explicit(),
            &json,
            &self.publish_relays,
        );
    }

    /// `J`/`K` — shift the selected timed event by `rel` × `repeat` × 5 min,
    /// following it onto another day if it crosses midnight.
    fn move_selected(&mut self, ctx: &mut AppContext<'_>, rel: i64) {
        let repeat = self.repeat.max(1) as i64;
        self.repeat = 1;
        let Some(i) = self.selected.filter(|&i| !self.blocks[i].all_day) else {
            return;
        };
        let delta = Duration::minutes(MOVE_MINUTES * repeat * rel);
        let (start, end) = (self.blocks[i].start + delta, self.blocks[i].end + delta);
        self.focus = with_date(self.focus, start.date_naive());
        self.cal_month = sidebar::month_of(self.focus);
        self.republish_times(ctx, i, start, end);
    }

    /// `v`/`V` — grow (`sign > 0`) or shrink the selected event's end by
    /// `repeat` × 5 min, never below a single 5-minute block.
    fn resize_selected(&mut self, ctx: &mut AppContext<'_>, sign: i64) {
        let repeat = self.repeat.max(1) as i64;
        self.repeat = 1;
        let Some(i) = self.selected.filter(|&i| !self.blocks[i].all_day) else {
            return;
        };
        let start = self.blocks[i].start;
        let end = self.blocks[i].end + Duration::minutes(MOVE_MINUTES * repeat * sign);
        // Keep at least one 5-minute block so the event never inverts.
        if end < start + Duration::minutes(MOVE_MINUTES) {
            return;
        }
        self.republish_times(ctx, i, start, end);
    }

    /// `T` — move the selected event to now (start snapped to 5 min), keeping
    /// its duration (viscal's `move_event_now`).
    fn move_selected_to_now(&mut self, ctx: &mut AppContext<'_>) {
        self.repeat = 1;
        let Some(i) = self.selected.filter(|&i| !self.blocks[i].all_day) else {
            return;
        };
        let start = snap_to_step(self.now);
        let end = start + (self.blocks[i].end - self.blocks[i].start);
        self.focus = start;
        self.cal_month = sidebar::month_of(start);
        self.republish_times(ctx, i, start, end);
    }

    /// Re-publish block `i` as a fresh kind-31923 addressable replacement over
    /// `[start, end)`, preserving its `d` tag, title, content and any other tags
    /// (location, hashtags…) so only the times change. Then optimistically
    /// update the block in place and keep it selected across the reload.
    fn republish_times(
        &mut self,
        ctx: &mut AppContext<'_>,
        i: usize,
        start: DateTime<Local>,
        end: DateTime<Local>,
    ) {
        let Some(new_id) = self.republish_block(ctx, i, start, end) else {
            return;
        };
        self.blocks.sort_by_key(|b| b.start);
        self.cursor = start;
        self.selected = self.blocks.iter().position(|b| b.id == new_id);
        self.pending_select = Some(new_id);
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// Re-publish block `i`'s time span, updating its `.id`/`.start`/`.end` in
    /// place. Returns the replacement event id, or `None` if it couldn't be
    /// written. Touches neither the selection nor the block ordering, so a
    /// cascade can re-publish several blocks by index and then re-sort / re-select
    /// exactly once (see [`apply_spans`]).
    fn republish_block(
        &mut self,
        ctx: &mut AppContext<'_>,
        i: usize,
        start: DateTime<Local>,
        end: DateTime<Local>,
    ) -> Option<[u8; 32]> {
        let overrides = [
            ("start", start.timestamp().to_string()),
            ("end", end.timestamp().to_string()),
        ];
        let new_id = self.republish_with(ctx, i, &overrides)?;
        let block = &mut self.blocks[i];
        block.start = start;
        block.end = end;
        // A re-time can change lane assignment, so drop the cached layout.
        self.invalidate_layout();
        Some(new_id)
    }

    /// Re-publish block `i` with its title changed, updating the block in place
    /// and keeping it selected across the reload. No move, so no re-sort needed.
    fn republish_title(&mut self, ctx: &mut AppContext<'_>, i: usize, title: &str) {
        let overrides = [("title", title.to_owned())];
        if self.republish_with(ctx, i, &overrides).is_some() {
            self.blocks[i].title = title.to_owned();
            self.pending_select = Some(self.blocks[i].id);
        }
    }

    /// `c`/`s` (from a blank buffer) or `A` (from the current title) — start an
    /// inline title edit of the selected event. No-op when nothing's selected.
    fn begin_edit(&mut self, clear: bool) {
        self.repeat = 1;
        let Some(i) = self.selected else {
            return;
        };
        let initial = if clear {
            String::new()
        } else {
            self.blocks[i].title.clone()
        };
        self.editing = Some(EditState {
            buf: initial.clone(),
            initial,
            focused: false,
            fresh: false,
        });
    }

    /// Finish an inline edit (Return / click-away): publish the new title when it
    /// changed and isn't blank; a blank buffer just keeps the existing title (so
    /// a fresh insert stays "New event").
    fn commit_edit(&mut self, ctx: &mut AppContext<'_>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let Some(i) = self.selected else {
            return;
        };
        let title = edit.buf.trim();
        if title.is_empty() || title == self.blocks[i].title {
            return;
        }
        self.republish_title(ctx, i, title);
    }

    /// Abort an inline edit (Escape): drop the buffer, and delete the event too
    /// if the edit was on a fresh insert (viscal's `cancel_editing` under
    /// `CAL_INSERTING`), since an unnamed just-inserted block is throwaway.
    fn cancel_edit(&mut self, ctx: &mut AppContext<'_>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        if edit.fresh
            && let Some(i) = self.selected
        {
            self.delete_block(ctx, i);
            self.reselect_at_cursor();
        }
    }

    /// Publish a kind-31923 addressable replacement of block `i`, copying every
    /// tag from the stored note except those named in `overrides` and re-adding
    /// each override as a single `[key, value]` tag — so the `d` tag, content and
    /// untouched tags carry over and only the overridden ones change. Ingests and
    /// fans it out, and points the in-memory block at the new id. Returns the new
    /// id, or `None` if it couldn't be written (watch-only account, the original
    /// note vanished, a build/frame failure).
    fn republish_with(
        &mut self,
        ctx: &mut AppContext<'_>,
        i: usize,
        overrides: &[(&str, String)],
    ) -> Option<[u8; 32]> {
        let Some(secret) = account_secret(ctx) else {
            tracing::warn!("horizon: can't edit an event with a watch-only account");
            return None;
        };

        // Build and frame the replacement while the txn (and the borrowed
        // original note) are alive, handing back only owned data.
        let framed = {
            let txn = match Transaction::new(ctx.ndb) {
                Ok(txn) => txn,
                Err(err) => {
                    tracing::error!("horizon: txn failed: {err}");
                    return None;
                }
            };
            let Ok(orig) = ctx.ndb.get_note_by_id(&txn, &self.blocks[i].id) else {
                tracing::error!("horizon: original note is gone; skipping edit");
                return None;
            };

            let mut builder = NoteBuilder::new()
                .content(orig.content())
                .kind(block::KIND_TIME_BASED as u32);
            // Copy every tag whose key we're not overriding…
            for tag in orig.tags() {
                if tag
                    .get_str(0)
                    .is_some_and(|k| overrides.iter().any(|(key, _)| *key == k))
                {
                    continue;
                }
                builder = builder.start_tag();
                for idx in 0..tag.count() {
                    if let Some(s) = tag.get_str(idx) {
                        builder = builder.tag_str(s);
                    }
                }
            }
            // …then add the overrides, so exactly one of each survives.
            for (key, value) in overrides {
                builder = builder.start_tag().tag_str(key).tag_str(value);
            }

            let Some(note) = builder.sign(&secret).build() else {
                tracing::error!("horizon: failed to build edited note");
                return None;
            };
            frame_note(&note).map(|json| (*note.id(), json))
        };
        let (new_id, json) = framed?;

        self.publish_json(ctx, json);
        self.blocks[i].id = new_id;
        Some(new_id)
    }

    /// `Ctrl-j` / `Ctrl-k` — push the selected event down / up by `repeat` × 5
    /// min, shoving any movable timed events it would collide with along with it
    /// so nothing ends up overlapping (viscal's `pushmove_down`/`pushmove_up`).
    /// A locked neighbour (viscal's `EV_IMMOVABLE`) is a wall: a step that would
    /// have to displace one is refused, stopping the push there.
    fn pushmove_selected(&mut self, ctx: &mut AppContext<'_>, dir: i64) {
        let repeat = self.repeat.max(1) as i64;
        self.repeat = 1;
        let Some(sel) = self.selected.filter(|&i| !self.blocks[i].all_day) else {
            return;
        };
        // Bail before planning if we couldn't publish the result anyway.
        if account_secret(ctx).is_none() {
            tracing::warn!("horizon: can't push an event with a watch-only account");
            return;
        }

        // Cascade one 5-minute step at a time over a scratch copy of the spans
        // (index-aligned with `blocks`), so a multi-step push stacks exactly as
        // repeating the keypress would and never leaps a block out of order.
        let mut spans = self.spans();
        let step = Duration::minutes(MOVE_MINUTES * dir);
        let mut moved = false;
        for _ in 0..repeat {
            let (ns, ne) = (spans[sel].start + step, spans[sel].end + step);
            let mut plan = match if dir > 0 {
                self.cascade_after(&spans, sel, ne)
            } else {
                self.cascade_before(&spans, sel, ns)
            } {
                Some(plan) => plan,
                None => break, // a locked block blocks this step
            };
            plan.push(SpanMove {
                index: sel,
                start: ns,
                end: ne,
            });
            for mv in plan {
                spans[mv.index] = Span {
                    start: mv.start,
                    end: mv.end,
                };
            }
            moved = true;
        }
        if moved {
            self.apply_spans(ctx, &spans, sel);
        }
    }

    /// `Ctrl-v` — grow the selected event's end by `repeat` × 5 min and push the
    /// movable events below it down to stay flush (viscal's
    /// `push_expand_selection`). Refused mid-way if a locked block is in the way.
    fn push_expand_selected(&mut self, ctx: &mut AppContext<'_>) {
        let repeat = self.repeat.max(1) as i64;
        self.repeat = 1;
        let Some(sel) = self.selected.filter(|&i| !self.blocks[i].all_day) else {
            return;
        };
        if account_secret(ctx).is_none() {
            tracing::warn!("horizon: can't expand an event with a watch-only account");
            return;
        }

        let mut spans = self.spans();
        let step = Duration::minutes(MOVE_MINUTES);
        let mut moved = false;
        for _ in 0..repeat {
            let ne = spans[sel].end + step;
            let Some(mut plan) = self.cascade_after(&spans, sel, ne) else {
                break; // a locked block below blocks the expansion
            };
            plan.push(SpanMove {
                index: sel,
                start: spans[sel].start,
                end: ne,
            });
            for mv in plan {
                spans[mv.index] = Span {
                    start: mv.start,
                    end: mv.end,
                };
            }
            moved = true;
        }
        if moved {
            self.apply_spans(ctx, &spans, sel);
        }
    }

    /// A scratch [`Span`] per block, index-aligned with [`Self::blocks`] (which
    /// is kept sorted by start), for planning a cascade before writing anything.
    fn spans(&self) -> Vec<Span> {
        self.blocks
            .iter()
            .map(|b| Span {
                start: b.start,
                end: b.end,
            })
            .collect()
    }

    /// Whether block `i` is locked against push-move (viscal's `EV_IMMOVABLE`).
    fn is_locked(&self, i: usize) -> bool {
        self.locked.contains(&self.blocks[i].uid)
    }

    /// Plan flushing the movable timed blocks after `from` downward so none
    /// starts before `floor`, stacking each flush against the previous. Stops at
    /// the first block already clear of `floor`. Returns the moves (empty if
    /// nothing collides), or `None` if a locked block would have to move.
    fn cascade_after(
        &self,
        spans: &[Span],
        from: usize,
        floor: DateTime<Local>,
    ) -> Option<Vec<SpanMove>> {
        let mut plan = Vec::new();
        let mut prev_end = floor;
        for (index, span) in spans.iter().enumerate().skip(from + 1) {
            if self.blocks[index].all_day {
                continue; // all-day events live in a separate row, never collide
            }
            if span.start >= prev_end {
                break; // a gap opens here, so the rest is undisturbed
            }
            if self.is_locked(index) {
                return None;
            }
            let end = prev_end + span.duration();
            plan.push(SpanMove {
                index,
                start: prev_end,
                end,
            });
            prev_end = end;
        }
        Some(plan)
    }

    /// Mirror of [`cascade_after`] for an upward push: flush the movable timed
    /// blocks before `from` up so none ends after `ceil`.
    fn cascade_before(
        &self,
        spans: &[Span],
        from: usize,
        ceil: DateTime<Local>,
    ) -> Option<Vec<SpanMove>> {
        let mut plan = Vec::new();
        let mut prev_start = ceil;
        for (index, span) in spans[..from].iter().enumerate().rev() {
            if self.blocks[index].all_day {
                continue;
            }
            if span.end <= prev_start {
                break;
            }
            if self.is_locked(index) {
                return None;
            }
            let start = prev_start - span.duration();
            plan.push(SpanMove {
                index,
                start,
                end: prev_start,
            });
            prev_start = start;
        }
        Some(plan)
    }

    /// Commit a planned cascade: re-publish every block whose span changed, then
    /// re-sort and settle the selection back on the block that started at `sel`
    /// (identified by its stable UID, since the re-publish minted new event ids).
    fn apply_spans(&mut self, ctx: &mut AppContext<'_>, spans: &[Span], sel: usize) {
        let sel_uid = self.blocks[sel].uid.clone();
        // Indices stay valid: `republish_block` updates in place without sorting.
        for (i, span) in spans.iter().enumerate() {
            if self.blocks[i].start != span.start || self.blocks[i].end != span.end {
                self.republish_block(ctx, i, span.start, span.end);
            }
        }
        self.blocks.sort_by_key(|b| b.start);
        self.selected = self.blocks.iter().position(|b| b.uid == sel_uid);
        if let Some(i) = self.selected {
            self.cursor = self.blocks[i].start;
            self.pending_select = Some(self.blocks[i].id);
        }
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// `l` — toggle the selected event's lock (viscal's `lock_selection`). A
    /// locked event won't be shoved by a neighbouring push-move.
    fn toggle_lock(&mut self) {
        self.repeat = 1;
        let Some(i) = self.selected else {
            return;
        };
        let uid = self.blocks[i].uid.clone();
        if !self.locked.remove(&uid) {
            self.locked.insert(uid);
        }
    }

    /// `x` — delete the selected event, then re-home the selection on whatever
    /// now sits under the cursor.
    fn delete_selected(&mut self, ctx: &mut AppContext<'_>) {
        self.repeat = 1;
        let Some(i) = self.selected else {
            return;
        };
        self.delete_block(ctx, i);
        self.reselect_at_cursor();
    }

    /// `dd` — delete the selected event and pull the same day's later events up
    /// by the deleted event's duration to close the gap (viscal's
    /// `delete_timeblock`).
    fn delete_timeblock(&mut self, ctx: &mut AppContext<'_>) {
        self.repeat = 1;
        let Some(i) = self.selected else {
            return;
        };
        let gone = self.blocks[i].clone();
        let span = gone.end - gone.start;

        // Snapshot the later same-day timed events *before* deleting, since the
        // delete and each pull-up re-index and re-sort `blocks`.
        let day = gone.start.date_naive();
        let later: Vec<[u8; 32]> = self
            .blocks
            .iter()
            .filter(|b| {
                b.id != gone.id && !b.all_day && b.start.date_naive() == day && b.start >= gone.end
            })
            .map(|b| b.id)
            .collect();

        self.delete_block(ctx, i);
        for id in later {
            if let Some(j) = self.blocks.iter().position(|b| b.id == id) {
                let (s, e) = (self.blocks[j].start - span, self.blocks[j].end - span);
                self.republish_times(ctx, j, s, e);
            }
        }

        // The pull-ups moved the cursor/selection around; settle it back on the
        // slot the deleted event vacated.
        self.cursor = gone.start;
        self.reselect_at_cursor();
        self.scroll_to_cursor = Some(Align::Center);
    }

    /// Publish a NIP-09 deletion for block `i`, drop it from the timeline, and
    /// remember its id so [`reload`] won't resurrect it (nostrdb keeps deleted
    /// notes). Leaves the selection for the caller to fix up.
    fn delete_block(&mut self, ctx: &mut AppContext<'_>, i: usize) {
        let Some(secret) = account_secret(ctx) else {
            tracing::warn!("horizon: can't delete an event with a watch-only account");
            return;
        };
        let block = self.blocks[i].clone();
        let author = *ctx.accounts.selected_account_pubkey();

        // NIP-09: reference the event id (`e`) and, since it's addressable, its
        // `kind:pubkey:d` address (`a`) plus the deleted kind (`k`).
        let addr = format!("{}:{}:{}", block::KIND_TIME_BASED, author.hex(), block.uid);
        let builder = NoteBuilder::new()
            .content("")
            .kind(5)
            .start_tag()
            .tag_str("e")
            .tag_str(&hex::encode(block.id))
            .start_tag()
            .tag_str("a")
            .tag_str(&addr)
            .start_tag()
            .tag_str("k")
            .tag_str(&block::KIND_TIME_BASED.to_string());

        let Some(note) = builder.sign(&secret).build() else {
            tracing::error!("horizon: failed to build deletion note");
            return;
        };
        let Some(json) = frame_note(&note) else {
            return;
        };
        self.publish_json(ctx, json);

        self.deleted.insert(block.id);
        self.blocks.remove(i);
        self.invalidate_layout();
    }

    /// Re-home the selection on whatever timed event overlaps the cursor window,
    /// clearing it when the cursor sits on empty grid. Also pins `pending_select`
    /// so the choice survives the reload our own delete/edit write triggers —
    /// [`reload`] otherwise resets the selection from `pending_select` alone.
    fn reselect_at_cursor(&mut self) {
        let end = self.cursor + Duration::minutes(SELECTION_MINUTES);
        self.selected = self.block_overlapping(self.cursor, end);
        self.pending_select = self.selected.map(|i| self.blocks[i].id);
    }

    /// First timed (non-all-day) block whose span overlaps `[start, end)`.
    fn block_overlapping(&self, start: DateTime<Local>, end: DateTime<Local>) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| !b.all_day && b.start < end && start < b.end)
    }
}

/// The inbound private-sync filter: this account's own NIP-52 calendar events.
fn calendar_sync_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds([block::KIND_DATE_BASED, block::KIND_TIME_BASED])
        .limit(5000)
        .build()
}

/// The selected account's signing key, or `None` for a watch-only account
/// (which can't create, edit or delete events).
fn account_secret(ctx: &AppContext<'_>) -> Option<[u8; 32]> {
    ctx.accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes())
}

/// Serialize a signed note into a `["EVENT", …]` client frame for ingestion and
/// relay publish, logging and returning `None` on the (unexpected) failure.
fn frame_note(note: &Note) -> Option<String> {
    match enostr::ClientMessage::event(note).and_then(|m| m.to_json()) {
        Ok(json) => Some(json),
        Err(err) => {
            tracing::error!("horizon: failed to frame note: {err}");
            None
        }
    }
}

/// A stable, unique `d`-tag (NIP-52 UID) for a newly created event. The block's
/// start time plus a nanosecond stamp keeps two same-second inserts distinct
/// without pulling in a random-id dependency.
fn unique_d_tag(start: DateTime<Local>) -> String {
    let nanos = Local::now().timestamp_nanos_opt().unwrap_or(0);
    format!("horizon-{}-{:x}", start.timestamp(), nanos)
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
        // `Button::fill()` silently re-enables the frame, so a bare
        // `.fill()` after `.frame(false)` would repaint the theme's border
        // stroke (and button padding) around the active tab. Re-assert
        // `.frame(false)`: the SURFACE fill is still painted, just without the
        // stray box, keeping the tab flush with its inactive siblings.
        btn = btn.fill(theme::SURFACE).frame(false);
    }
    ui.add(btn).clicked()
}

/// First cell of the month grid containing `dt`: the Sunday on or before the
/// 1st of that month (Sunday-first, matching the sidebar mini-month).
fn month_grid_start(dt: DateTime<Local>) -> NaiveDate {
    let first = dt
        .date_naive()
        .with_day(1)
        .unwrap_or_else(|| dt.date_naive());
    let back = first.weekday().num_days_from_sunday() as i64;
    first - Duration::days(back)
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

#[cfg(test)]
mod tests {
    use super::*;
    use block::Block;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 6, 25, h, m, 0).unwrap()
    }

    fn timed(uid: &str, s: (u32, u32), e: (u32, u32)) -> Block {
        Block::timed([0; 32], uid, "x", at(s.0, s.1), at(e.0, e.1))
    }

    /// Flatten a plan to `(index, start, end)` tuples for terse assertions.
    fn as_tuples(plan: &[SpanMove]) -> Vec<(usize, DateTime<Local>, DateTime<Local>)> {
        plan.iter().map(|m| (m.index, m.start, m.end)).collect()
    }

    /// A `Horizon` seeded with `blocks` (sorted by start, as [`reload`] leaves
    /// them) so the cascade planners can be exercised without a db or account.
    fn horizon_with(mut blocks: Vec<Block>) -> Horizon {
        blocks.sort_by_key(|b| b.start);
        Horizon {
            blocks,
            ..Default::default()
        }
    }

    #[test]
    fn cascade_after_stacks_neighbours_flush() {
        // a 9–10, b 10–11, c 11–11:30, back to back. Pushing a's end to 10:30
        // shoves b and c down to stay flush.
        let h = horizon_with(vec![
            timed("a", (9, 0), (10, 0)),
            timed("b", (10, 0), (11, 0)),
            timed("c", (11, 0), (11, 30)),
        ]);
        let plan = h.cascade_after(&h.spans(), 0, at(10, 30)).unwrap();
        assert_eq!(
            as_tuples(&plan),
            vec![(1, at(10, 30), at(11, 30)), (2, at(11, 30), at(12, 0)),]
        );
    }

    #[test]
    fn cascade_after_stops_at_a_gap() {
        // A gap after the floor leaves the later event undisturbed.
        let h = horizon_with(vec![
            timed("a", (9, 0), (10, 0)),
            timed("b", (11, 0), (12, 0)),
        ]);
        let plan = h.cascade_after(&h.spans(), 0, at(10, 30)).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn cascade_after_refused_by_a_locked_block() {
        let mut h = horizon_with(vec![
            timed("a", (9, 0), (10, 0)),
            timed("b", (10, 0), (11, 0)),
        ]);
        h.locked.insert("b".to_owned());
        assert!(h.cascade_after(&h.spans(), 0, at(10, 30)).is_none());
    }

    #[test]
    fn cascade_before_stacks_neighbours_flush() {
        // Pushing b up so it starts at 9:30 pulls a up to end at 9:30.
        let h = horizon_with(vec![
            timed("a", (9, 0), (10, 0)),
            timed("b", (10, 0), (11, 0)),
        ]);
        let plan = h.cascade_before(&h.spans(), 1, at(9, 30)).unwrap();
        assert_eq!(as_tuples(&plan), vec![(0, at(8, 30), at(9, 30))]);
    }

    #[test]
    fn cascade_before_refused_by_a_locked_block() {
        let mut h = horizon_with(vec![
            timed("a", (9, 0), (10, 0)),
            timed("b", (10, 0), (11, 0)),
        ]);
        h.locked.insert("a".to_owned());
        assert!(h.cascade_before(&h.spans(), 1, at(9, 30)).is_none());
    }
}
