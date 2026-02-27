use enostr::Pubkey;
use nostrdb::Note;
use rustc_hash::FxHashMap;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel as chan;

use nostrdb::{Filter, Ndb, Transaction};
use notedeck::{AppContext, AppResponse};

use chrono::{Datelike, TimeZone, Utc};

mod chart;
mod sparkline;
mod ui;

// ----------------------
// Worker protocol
// ----------------------

#[derive(Debug)]
enum WorkerCmd {
    Refresh,
    //Quit,
}

// Buckets are multiples of time ranges
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Daily,
    Weekly,
    Monthly,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Daily, Period::Weekly, Period::Monthly];

    pub fn label(self) -> &'static str {
        match self {
            Period::Daily => "day",
            Period::Weekly => "week",
            Period::Monthly => "month",
        }
    }
}

/// All the data we are interested in for a specific range
#[derive(Default, Clone, Debug)]
struct Bucket {
    pub total: u64,
    pub kinds: rustc_hash::FxHashMap<u64, u32>,
    pub clients: rustc_hash::FxHashMap<String, u32>,
    pub kind1_authors: rustc_hash::FxHashMap<Pubkey, u32>,
}

fn note_client_tag<'a>(note: &Note<'a>) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some("client") = tag.get_str(0) else {
            continue;
        };

        return tag.get_str(1);
    }

    None
}

impl Bucket {
    #[inline(always)]
    pub fn bump(&mut self, note: &Note<'_>) {
        self.total += 1;
        let kind = note.kind();
        *self.kinds.entry(kind as u64).or_default() += 1;

        // Track kind1 authors
        if kind == 1 {
            let pk = Pubkey::new(*note.pubkey());
            *self.kind1_authors.entry(pk).or_default() += 1;
        }

        if let Some(client) = note_client_tag(note) {
            *self.clients.entry(client.to_string()).or_default() += 1;
        } else {
            // TODO(jb55): client fingerprinting ?
        }
    }
}

// bucket_end_ts(idx) - self.bucket_size_secs
#[derive(Debug, Clone, Default)]
struct RollingCache {
    pub bucket_size_secs: i64,
    pub anchor_end_ts: i64,
    pub buckets: Vec<Bucket>,
}

impl RollingCache {
    pub fn bucket_end_ts(&self, idx: usize) -> i64 {
        self.anchor_end_ts - (idx as i64) * self.bucket_size_secs
    }

    pub fn bucket_start_ts(&self, idx: usize) -> i64 {
        self.bucket_end_ts(idx) - self.bucket_size_secs
    }

    pub fn daily(now_ts: i64, days: usize) -> Self {
        let day_anchor = next_midnight_utc(now_ts);

        Self {
            bucket_size_secs: 86_400,
            anchor_end_ts: day_anchor,
            buckets: vec![Bucket::default(); days],
        }
    }

    pub fn weekly(now_ts: i64, weeks: usize, week_starts_monday: bool) -> Self {
        let anchor_end_ts = next_week_boundary_utc(now_ts, week_starts_monday);
        Self {
            bucket_size_secs: 7 * 86_400,
            anchor_end_ts,
            buckets: vec![Bucket::default(); weeks],
        }
    }

    // “month-ish” (30d buckets) but aligned so bucket 0 ends at the next month boundary
    pub fn monthly_30d(now_ts: i64, months: usize) -> Self {
        let anchor_end_ts = next_month_boundary_utc(now_ts);
        Self {
            bucket_size_secs: 30 * 86_400,
            anchor_end_ts,
            buckets: vec![Bucket::default(); months],
        }
    }

    #[inline(always)]
    pub fn bump(&mut self, note: &Note<'_>) {
        let ts = note.created_at() as i64;

        // bucket windows are [end-(i+1)*size, end-i*size)
        // so treat `end` itself as "future"
        let delta = (self.anchor_end_ts - 1) - ts;

        if delta < 0 {
            return; // ignore future timestamps
        }

        let idx = (delta / self.bucket_size_secs) as usize;
        if idx >= self.buckets.len() {
            return; // outside window
        }

        self.buckets[idx].bump(note);
    }
}

#[derive(Clone, Debug, Default)]
struct DashboardState {
    total: Bucket,
    daily: RollingCache,
    weekly: RollingCache,
    monthly: RollingCache,
}

#[derive(Debug, Clone)]
struct Snapshot {
    started_at: Instant,
    snapshot_at: Instant,
    state: DashboardState,
}

#[derive(Debug)]
enum WorkerMsg {
    Snapshot(Snapshot),
    Finished {
        started_at: Instant,
        finished_at: Instant,
        state: DashboardState,
    },
    Failed {
        started_at: Instant,
        finished_at: Instant,
        error: String,
    },
}

// ----------------------
// Dashboard (single pass, single worker)
// ----------------------

pub struct Dashboard {
    initialized: bool,

    // Worker channels
    cmd_tx: Option<chan::Sender<WorkerCmd>>,
    msg_rx: Option<chan::Receiver<WorkerMsg>>,

    // Refresh policy
    refresh_every: Duration,
    next_tick: Instant,

    // Global UI controls
    period: Period,

    // UI state (progressively filled via snapshots)
    running: bool,

    last_started: Option<Instant>,
    last_snapshot: Option<Instant>,
    last_finished: Option<Instant>,
    last_duration: Option<Duration>,
    last_error: Option<String>,

    state: DashboardState,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            initialized: false,

            period: Period::Weekly,

            cmd_tx: None,
            msg_rx: None,

            refresh_every: Duration::from_secs(300),
            next_tick: Instant::now(),

            running: false,
            last_started: None,
            last_snapshot: None,
            last_finished: None,
            last_duration: None,
            last_error: None,

            state: DashboardState::default(),
        }
    }
}

impl notedeck::App for Dashboard {
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        if !self.initialized {
            self.initialized = true;
            self.init(egui_ctx.clone(), ctx);
        }

        self.process_worker_msgs();
        self.schedule_refresh();
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.show(ui, ctx);
        AppResponse::none()
    }
}

impl Dashboard {
    fn selected_cache(&self) -> &RollingCache {
        match self.period {
            Period::Daily => &self.state.daily,
            Period::Weekly => &self.state.weekly,
            Period::Monthly => &self.state.monthly,
        }
    }

    fn init(&mut self, egui_ctx: egui::Context, ctx: &mut AppContext<'_>) {
        // spawn single worker thread and keep it alive
        let (cmd_tx, cmd_rx) = chan::unbounded::<WorkerCmd>();
        let (msg_tx, msg_rx) = chan::unbounded::<WorkerMsg>();

        self.cmd_tx = Some(cmd_tx.clone());
        self.msg_rx = Some(msg_rx);

        // Clone the DB handle into the worker thread (Ndb is typically cheap/cloneable)
        let ndb = ctx.ndb.clone();

        spawn_worker(egui_ctx, ndb, cmd_rx, msg_tx);

        // kick the first run immediately
        let _ = cmd_tx.send(WorkerCmd::Refresh);
        self.running = true;
        self.last_error = None;
        self.last_started = Some(Instant::now());
        self.last_snapshot = None;
        self.last_finished = None;
        self.last_duration = None;
        self.state = DashboardState::default();
    }

    fn process_worker_msgs(&mut self) {
        let Some(rx) = &self.msg_rx else { return };

        let mut got_any = false;

        while let Ok(msg) = rx.try_recv() {
            got_any = true;
            match msg {
                WorkerMsg::Snapshot(s) => {
                    self.running = true;
                    self.last_started = Some(s.started_at);
                    self.last_snapshot = Some(s.snapshot_at);
                    self.last_error = None;

                    self.state = s.state;
                }
                WorkerMsg::Finished {
                    started_at,
                    finished_at,
                    state,
                } => {
                    self.running = false;
                    self.last_started = Some(started_at);
                    self.last_snapshot = Some(finished_at);
                    self.last_finished = Some(finished_at);
                    self.last_duration = Some(finished_at.saturating_duration_since(started_at));
                    self.last_error = None;

                    self.state = state;
                }
                WorkerMsg::Failed {
                    started_at,
                    finished_at,
                    error,
                } => {
                    self.running = false;
                    self.last_started = Some(started_at);
                    self.last_snapshot = Some(finished_at);
                    self.last_finished = Some(finished_at);
                    self.last_duration = Some(finished_at.saturating_duration_since(started_at));
                    self.last_error = Some(error);
                }
            }
        }

        if got_any {
            // No-op; we already requested repaint on every message.
        }
    }

    fn schedule_refresh(&mut self) {
        // throttle scheduling checks a bit
        let now = Instant::now();
        if now < self.next_tick {
            return;
        }
        self.next_tick = now + Duration::from_millis(200);

        if self.running {
            return;
        }

        // refresh every 30 seconds from the last finished time (or from init)
        let last = self
            .last_finished
            .or(self.last_started)
            .unwrap_or_else(Instant::now);

        if now.saturating_duration_since(last) >= self.refresh_every
            && let Some(tx) = &self.cmd_tx
        {
            // reset UI fields for progressive load, but keep old values visible until snapshots arrive
            self.running = true;
            self.last_error = None;
            self.last_started = Some(now);
            self.last_snapshot = None;
            self.last_finished = None;
            self.last_duration = None;
            self.state = DashboardState::default();

            let _ = tx.send(WorkerCmd::Refresh);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, ctx: &mut AppContext<'_>) {
        crate::ui::dashboard_ui(self, ui, ctx);
    }
}

// ----------------------
// Worker side (single pass, periodic snapshots)
// ----------------------

fn spawn_worker(
    ctx: egui::Context,
    ndb: Ndb,
    cmd_rx: chan::Receiver<WorkerCmd>,
    msg_tx: chan::Sender<WorkerMsg>,
) {
    thread::Builder::new()
        .name("dashboard-worker".to_owned())
        .spawn(move || {
            let mut should_quit = false;

            while !should_quit {
                match cmd_rx.recv() {
                    Ok(WorkerCmd::Refresh) => {
                        let started_at = Instant::now();

                        match materialize_single_pass(&ctx, &ndb, &msg_tx, started_at) {
                            Ok(state) => {
                                let _ = msg_tx.send(WorkerMsg::Finished {
                                    started_at,
                                    finished_at: Instant::now(),
                                    state,
                                });
                            }
                            Err(e) => {
                                let _ = msg_tx.send(WorkerMsg::Failed {
                                    started_at,
                                    finished_at: Instant::now(),
                                    error: format!("{e:?}"),
                                });
                            }
                        }
                    }
                    Err(_) => {
                        should_quit = true;
                    }
                }
            }
        })
        .expect("failed to spawn dashboard worker thread");
}

struct Acc {
    last_emit: Instant,

    state: DashboardState,
}

fn materialize_single_pass(
    ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<WorkerMsg>,
    started_at: Instant,
) -> Result<DashboardState, nostrdb::Error> {
    // one transaction per refresh run
    let txn = Transaction::new(ndb)?;

    // all notes
    let filters = vec![Filter::new_with_capacity(1).build()];

    let days = 14;
    let weeks = 12;
    let months = 12;
    let week_starts_monday = true;

    let now = Utc::now().timestamp();

    let mut acc = Acc {
        last_emit: Instant::now(),
        state: DashboardState {
            total: Bucket::default(),
            daily: RollingCache::daily(now, days),
            weekly: RollingCache::weekly(now, weeks, week_starts_monday),
            monthly: RollingCache::monthly_30d(now, months),
        },
    };

    let emit_every = Duration::from_millis(32);

    let _ = ndb.fold(&txn, &filters, &mut acc, |acc, note| {
        acc.state.total.bump(&note);
        acc.state.daily.bump(&note);
        acc.state.weekly.bump(&note);
        acc.state.monthly.bump(&note);

        let now = Instant::now();
        if now.saturating_duration_since(acc.last_emit) >= emit_every {
            acc.last_emit = now;

            let _ = msg_tx.send(WorkerMsg::Snapshot(Snapshot {
                started_at,
                snapshot_at: now,
                state: acc.state.clone(),
            }));

            ctx.request_repaint();
        }

        acc
    });

    Ok(acc.state)
}

fn next_midnight_utc(now_ts: i64) -> i64 {
    let dt = Utc.timestamp_opt(now_ts, 0).single().unwrap();
    let tomorrow = dt.date_naive().succ_opt().unwrap();
    Utc.from_utc_datetime(&tomorrow.and_hms_opt(0, 0, 0).unwrap())
        .timestamp()
}

fn next_week_boundary_utc(now_ts: i64, starts_monday: bool) -> i64 {
    let dt = Utc.timestamp_opt(now_ts, 0).single().unwrap();
    let today = dt.date_naive();

    let start = if starts_monday {
        chrono::Weekday::Mon
    } else {
        chrono::Weekday::Sun
    };
    let weekday = today.weekday();

    // days until next week start (0..6); if today is start, boundary is next week start (7 days)
    let mut delta =
        (7 + (start.num_days_from_monday() as i32) - (weekday.num_days_from_monday() as i32)) % 7;
    if delta == 0 {
        delta = 7;
    }

    let next = today + chrono::Duration::days(delta as i64);
    Utc.from_utc_datetime(&next.and_hms_opt(0, 0, 0).unwrap())
        .timestamp()
}

fn next_month_boundary_utc(now_ts: i64) -> i64 {
    let dt = Utc.timestamp_opt(now_ts, 0).single().unwrap();
    let y = dt.year();
    let m = dt.month();

    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp()
}

fn top_kinds_over(cache: &RollingCache, limit: usize) -> Vec<(u64, u64)> {
    let mut agg: FxHashMap<u64, u64> = Default::default();

    for b in &cache.buckets {
        for (kind, count) in &b.kinds {
            *agg.entry(*kind).or_default() += *count as u64;
        }
    }

    let mut v: Vec<_> = agg.into_iter().collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(limit);
    v
}

pub(crate) fn top_kind1_authors_over(cache: &RollingCache, limit: usize) -> Vec<(Pubkey, u64)> {
    let mut agg: FxHashMap<Pubkey, u64> = Default::default();
    for b in &cache.buckets {
        for (pubkey, count) in &b.kind1_authors {
            *agg.entry(*pubkey).or_default() += *count as u64;
        }
    }
    let mut v: Vec<_> = agg.into_iter().collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    v.truncate(limit);
    v
}
