use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel as chan;

use nostrdb::{Filter, Ndb, Transaction};
use notedeck::{AppContext, AppResponse, try_process_events_core};

mod chart;
mod ui;

// ----------------------
// Worker protocol
// ----------------------

#[derive(Debug)]
enum WorkerCmd {
    Refresh,
    //Quit,
}

#[derive(Clone, Debug, Default)]
struct DashboardState {
    total_count: usize,
    top_kinds: Vec<(u32, u64)>,
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

            cmd_tx: None,
            msg_rx: None,

            refresh_every: Duration::from_secs(10),
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
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        try_process_events_core(ctx, ui.ctx(), |_, _| {});

        if !self.initialized {
            self.initialized = true;
            self.init(ctx);
        }

        self.process_worker_msgs(ui.ctx());
        self.schedule_refresh();

        self.show(ui);

        AppResponse::none()
    }
}

impl Dashboard {
    fn init(&mut self, ctx: &mut AppContext<'_>) {
        // spawn single worker thread and keep it alive
        let (cmd_tx, cmd_rx) = chan::unbounded::<WorkerCmd>();
        let (msg_tx, msg_rx) = chan::unbounded::<WorkerMsg>();

        self.cmd_tx = Some(cmd_tx.clone());
        self.msg_rx = Some(msg_rx);

        // Clone the DB handle into the worker thread (Ndb is typically cheap/cloneable)
        let ndb = ctx.ndb.clone();

        spawn_worker(ndb, cmd_rx, msg_tx);

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

    fn process_worker_msgs(&mut self, egui_ctx: &egui::Context) {
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

                    // Push UI updates with no "loading screen"
                    egui_ctx.request_repaint();
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

                    egui_ctx.request_repaint();
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

                    egui_ctx.request_repaint();
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

    fn show(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.grid(ui);
                });
            });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let cols = 2;
        let min_card = 240.0;

        egui::Grid::new("dashboard_grid_single_worker")
            .num_columns(cols)
            .min_col_width(min_card)
            .spacing(egui::vec2(8.0, 8.0))
            .show(ui, |ui| {
                use crate::ui::{card_ui, kinds_ui, totals_ui};

                // Card 1: Total notes
                card_ui(ui, min_card, |ui| {
                    totals_ui(self, ui);
                });

                // Card 2: Kinds (top)
                card_ui(ui, min_card, |ui| {
                    kinds_ui(self, ui);
                });

                ui.end_row();
            });
    }
}

// ----------------------
// Worker side (single pass, periodic snapshots)
// ----------------------

fn spawn_worker(ndb: Ndb, cmd_rx: chan::Receiver<WorkerCmd>, msg_tx: chan::Sender<WorkerMsg>) {
    thread::Builder::new()
        .name("dashboard-worker".to_owned())
        .spawn(move || {
            let mut should_quit = false;

            while !should_quit {
                match cmd_rx.recv() {
                    Ok(WorkerCmd::Refresh) => {
                        let started_at = Instant::now();

                        match materialize_single_pass(&ndb, &msg_tx, started_at) {
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

fn materialize_single_pass(
    ndb: &Ndb,
    msg_tx: &chan::Sender<WorkerMsg>,
    started_at: Instant,
) -> Result<DashboardState, nostrdb::Error> {
    // one transaction per refresh run
    let txn = Transaction::new(ndb)?;

    // all notes
    let filters = vec![Filter::new_with_capacity(1).build()];

    struct Acc {
        total_count: usize,
        kinds: HashMap<u32, u64>,
        last_emit: Instant,
    }

    let mut acc = Acc {
        total_count: 0,
        kinds: HashMap::new(),
        last_emit: Instant::now(),
    };

    let emit_every = Duration::from_millis(32);

    let _ = ndb.fold(&txn, &filters, &mut acc, |acc, note| {
        acc.total_count += 1;

        *acc.kinds.entry(note.kind()).or_default() += 1;

        let now = Instant::now();
        if now.saturating_duration_since(acc.last_emit) >= emit_every {
            acc.last_emit = now;

            let top = top_kinds(&acc.kinds, 6);
            let _ = msg_tx.send(WorkerMsg::Snapshot(Snapshot {
                started_at,
                snapshot_at: now,
                state: DashboardState {
                    total_count: acc.total_count,
                    top_kinds: top,
                },
            }));
        }

        acc
    });

    Ok(DashboardState {
        total_count: acc.total_count,
        top_kinds: top_kinds(&acc.kinds, 6),
    })
}

fn top_kinds(hmap: &HashMap<u32, u64>, limit: usize) -> Vec<(u32, u64)> {
    let mut v: Vec<(u32, u64)> = hmap.iter().map(|(k, c)| (*k, *c)).collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(limit);
    v
}
