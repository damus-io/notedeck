//! Where the app's books come from: the journal under `$CASHFLOW_DIR` and the
//! vault's edits, loaded and derived off the UI thread and kept current.
//!
//! [`JournalSource`] is polled once a frame from `App::update`. It never
//! blocks: the load, every re-derive and every rescan run as one
//! [`LoadJob`] on the host's `JobPool`, and a sync runs on a thread of its
//! own since it waits on the network. Each frame it:
//!
//! 1. takes a finished job's result: the raw ledger back, and new books if
//!    the job derived any;
//! 2. writes the layer flips [`Layers`] queued to the vault, then polls the
//!    [`EditsCache`], and when a vault record changed re-reads the home
//!    commodity, the layer switches and which segments the vault lists that
//!    the journal lacks;
//! 3. starts the next job when nothing is running: a derive when the edits,
//!    the home commodity or the [`Layers`] differ from what the books were
//!    derived with, else a rescan every [`RESCAN_EVERY`].
//!
//! A job that derives also works out the books' [`Triage`] from the raw
//! ledger and the same edits, so the inbox never groups or suggests on the
//! UI thread. When a vault record changes the chart's [`Labels`] are
//! re-read too.
//!
//! The raw ledger is kept between jobs, so a rescan that finds a segment
//! (from `cashflow import`, rsync, git, anything) merges just that one in
//! with [`ledgerdb::Ledger::refresh`], and an edit re-derives reusing the
//! previous books' rule matches ([`Books::from_raw`]). Only a change to the
//! layer filter or the vault's retired set reloads every segment.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cashflow::chart::{Chart, Labels};
use cashflow::settings::Setting;
use cashflow::sync::{self, SyncJob, SyncReport, TransportInfo, TransportReport};
use cashflow::triage::Triage;
use cashflow::zone::Zone;
use cashflow::{Books, CashflowDir, Config, DeriveInputs, EditsCache, Settings, Vault, manifest};
use ledgerdb::{Derived, JournalStore, LayerFilter, Ledger, LoadReport, SegmentHash};
use nostrdb::Transaction;
use notedeck::AppContext;
use poll_promise::Promise;
use tokio::sync::oneshot;

use crate::Layers;
use crate::boards::ProjectLink;

/// How often an idle source lists the journal directory for segments that
/// arrived by some other route than this app.
pub const RESCAN_EVERY: Duration = Duration::from_secs(3);

/// The raw ledger, and the retired set it was loaded without.
pub struct RawLedger {
    /// Every segment the filter includes, merged, before any edit.
    ledger: Ledger,
    /// The segments it leaves out because the vault retires them. A change
    /// to this set means reloading: `refresh` only ever adds segments.
    retired: HashSet<SegmentHash>,
}

/// One load, derive or rescan, run on the job pool.
pub struct LoadJob {
    /// The journal.
    pub store: JournalStore,
    /// The raw ledger the last job returned, refreshed if it still fits
    /// [`Self::filter`] and the retired set, else replaced by a full load.
    pub raw: Option<RawLedger>,
    /// The layers to load.
    pub filter: LayerFilter,
    /// The vault's edits and retired segments.
    pub inputs: Arc<DeriveInputs>,
    /// The books in hand's derive, whose rule matches a re-derive reuses.
    pub previous: Option<Arc<Derived>>,
    /// The home commodity's name.
    pub home: String,
    /// Derive even when the refresh found nothing new: the edits, the home
    /// commodity or the filter changed. A plain rescan leaves it off.
    pub derive: bool,
    /// When today began (unix seconds), what the triage counts "today"
    /// from.
    pub since: u64,
}

/// Books a job derived, and their triage.
pub struct Loaded {
    /// The books.
    pub books: Books,
    /// What is left to categorize in them, worked out with the same edits.
    pub triage: Triage,
}

/// What a [`LoadJob`] did.
pub struct LoadDone {
    /// The raw ledger, to hand to the next job; `None` after an error.
    pub raw: Option<RawLedger>,
    /// New books and their triage, when the job derived; `None` when a
    /// rescan found nothing new or the store couldn't be listed.
    pub loaded: Option<Loaded>,
    /// Every segment in the store, sorted, as the job listed it.
    pub local: Vec<SegmentHash>,
    /// Why the store couldn't be listed.
    pub error: Option<String>,
    /// How long the job took.
    pub elapsed: Duration,
}

impl LoadJob {
    /// Refresh or load the raw ledger, then derive if anything changed.
    /// Blocking: run it off the UI thread.
    #[profiling::function]
    pub fn run(self) -> LoadDone {
        let started = Instant::now();
        let failed = |err: ledgerdb::StoreError| LoadDone {
            raw: None,
            loaded: None,
            local: Vec::new(),
            error: Some(err.to_string()),
            elapsed: started.elapsed(),
        };
        let local = match self.store.list() {
            Ok(local) => local,
            Err(err) => return failed(err),
        };
        let fits = |raw: &RawLedger| {
            raw.ledger.filter() == &self.filter && raw.retired == self.inputs.retired
        };
        let refreshed = match self.raw.filter(fits) {
            Some(mut raw) => raw.ledger.refresh(&self.store).map(|report| (raw, report)),
            None => Ledger::load_without(&self.store, self.filter, self.inputs.retired.clone())
                .map(|loaded| {
                    let raw = RawLedger {
                        ledger: loaded.ledger,
                        retired: self.inputs.retired.clone(),
                    };
                    (raw, loaded.report)
                }),
        };
        let (raw, report): (RawLedger, LoadReport) = match refreshed {
            Ok(refreshed) => refreshed,
            Err(err) => return failed(err),
        };
        let grew = !report.loaded.is_empty();
        let loaded = (self.derive || grew).then(|| {
            let books = Books::from_raw(
                &raw.ledger,
                report,
                &self.inputs.edits,
                self.previous.as_deref(),
                &self.home,
            );
            let triage = Triage::new(Some(&raw.ledger), &books, self.inputs.clone(), self.since);
            Loaded { books, triage }
        });
        LoadDone {
            raw: Some(raw),
            loaded,
            local,
            error: None,
            elapsed: started.elapsed(),
        }
    }
}

/// What the books in hand were derived with, as counters that move when
/// their inputs change, so comparing them each frame copies nothing. A job
/// is due when the wanted one differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DerivedWith {
    /// The account whose vault the edits came from.
    author: [u8; 32],
    /// [`EditsCache::generation`] at the time.
    edits: u64,
    /// [`JournalSource::home_generation`] at the time.
    home: u64,
    /// [`Layers::generation`] at the time.
    layers: u64,
}

/// Where the journal stands, for the shell to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalState {
    /// The first load hasn't finished.
    Loading,
    /// The journal directory has no segments.
    Empty,
    /// The journal couldn't be listed; the message says why.
    Error,
    /// Books are loaded.
    Ready,
}

/// The segments the vault lists that the journal lacks.
#[derive(Debug, Default)]
pub struct MissingSegments {
    /// Their hashes, by host then oldest import first: what a sync fetches.
    pub hashes: Vec<SegmentHash>,
    /// One line naming how many and where they are, written when they
    /// change so a frame draws it without formatting.
    pub summary: String,
}

/// How the last sync went, written once when it finished.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// Whether any transport failed a step or served a bad file.
    pub failed: bool,
    /// One line on what moved, or the first error.
    pub summary: String,
}

/// One configured transport, and how its part of the last sync went.
#[derive(Debug)]
pub struct TransportStatus {
    /// What `config.toml` says about it, and whether its program is here.
    pub info: TransportInfo,
    /// Its part of the last sync this app ran, if it ran one.
    pub last: Option<TransportRun>,
}

/// One transport's part of a sync, written once when the sync finished.
#[derive(Debug)]
pub struct TransportRun {
    /// When the sync finished, as local `HH:MM`.
    pub at: String,
    /// "fetched 2, admitted 2, rejected 0, pushed 5".
    pub counts: String,
    /// What failed, if anything did.
    pub error: Option<String>,
}

impl TransportRun {
    /// `report`'s counts and first error, stamped `at`.
    fn of(report: &TransportReport, at: &str) -> Self {
        TransportRun {
            at: at.to_owned(),
            counts: format!(
                "fetched {}, admitted {}, rejected {}, pushed {}",
                report.fetched.len(),
                report.admit.admitted.len(),
                report.admit.rejected.len(),
                report.pushed.len()
            ),
            error: report.errors.first().map(|err| err.to_string()),
        }
    }
}

/// The journal and vault behind a [`crate::CashflowApp`], loaded and kept
/// current. See the module docs.
pub struct JournalSource {
    /// `$CASHFLOW_DIR`.
    dir: CashflowDir,
    /// The journal in it.
    store: JournalStore,
    /// Where the journal is, written once for the empty pane to draw.
    journal_path: String,
    /// The selected account's vault fold; replaced when the account changes.
    edits: Option<EditsCache>,
    /// The raw ledger, here between jobs and away while one runs.
    raw: Option<RawLedger>,
    /// The running job.
    job: Option<oneshot::Receiver<LoadDone>>,
    /// What the running job derives with, once it is done.
    job_with: Option<DerivedWith>,
    /// What the books in hand were derived with.
    derived_with: Option<DerivedWith>,
    /// When the last job finished, for the rescan timer.
    last_scan: Option<Instant>,
    /// The store's segments as the last job listed them, sorted.
    local: Vec<SegmentHash>,
    /// The vault's home commodity.
    home: String,
    /// Bumped whenever [`Self::home`] changes.
    home_generation: u64,
    /// The synced time zone a note's `created_at` is dated in.
    zone: Zone,
    /// The last job's listing error.
    error: Option<String>,
    /// What the vault lists that the journal lacks.
    missing: MissingSegments,
    /// "2 segments here are retired by the vault, …", or empty: written
    /// with [`Self::missing`].
    retired_here: String,
    /// Whether [`Self::missing`] needs comparing again.
    missing_stale: bool,
    /// The `[[sync.transport]]`s `config.toml` lists, or why it doesn't
    /// read.
    transports: Result<Vec<TransportStatus>, String>,
    /// The running sync.
    sync: Option<Promise<SyncReport>>,
    /// How the last sync went.
    last_sync: Option<SyncOutcome>,
    /// How long the last derive's job took.
    load_time: Option<Duration>,
    /// The chart's display names and icons, re-read when the vault changes.
    labels: Arc<Labels>,
    /// Which Headway boards each project links, re-read when the vault
    /// changes.
    project_links: Arc<[ProjectLink]>,
}

impl JournalSource {
    /// A source over the journal in `dir`. Nothing is read until the first
    /// [`Self::update`].
    pub fn new(dir: CashflowDir) -> Self {
        let transports = transport_status(&dir, Vec::new());
        Self {
            store: cashflow::journal::store(&dir),
            journal_path: dir.journal().to_string_lossy().into_owned(),
            dir,
            edits: None,
            raw: None,
            job: None,
            job_with: None,
            derived_with: None,
            last_scan: None,
            local: Vec::new(),
            home: cashflow::settings::DEFAULT_HOME_COMMODITY.to_owned(),
            home_generation: 0,
            zone: Zone::Local,
            error: None,
            missing: MissingSegments::default(),
            retired_here: String::new(),
            missing_stale: true,
            transports,
            sync: None,
            last_sync: None,
            load_time: None,
            labels: Arc::new(Labels::default()),
            project_links: Arc::from([]),
        }
    }

    /// One frame's worth of work: take what finished, write `layers`' flips
    /// to the vault and switch them as the vault says, and start the next
    /// job if one is due, loading the layers `layers` leaves on and counting
    /// the triage's "today" from `since` (unix seconds). Returns new books
    /// when a job derived some; `books` are the ones in hand, which a
    /// re-derive reuses.
    #[profiling::function]
    pub fn update(
        &mut self,
        ctx: &mut AppContext<'_>,
        layers: &mut Layers,
        books: Option<&Books>,
        since: u64,
    ) -> Option<Loaded> {
        let fresh = self.take_job();
        self.take_sync();

        let author = *ctx.accounts.selected_account_pubkey();
        if let Some(full) = ctx.accounts.selected_filled() {
            let secret = full.secret_key.secret_bytes();
            ctx.register_team_root(cashflow::vault::vault_root(&secret));
            write_layers(layers, &secret, ctx);
        }
        if self.edits.as_ref().is_none_or(|c| c.author() != &author) {
            // Before the txn the first poll reads: see `EditsCache::new`.
            self.edits = Some(EditsCache::new(ctx.ndb, author));
        }
        let edits = self.edits.as_mut().expect("just set");

        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return fresh;
        };
        if edits.poll(ctx.ndb, &txn).vault {
            self.labels = Arc::new(Chart::load(ctx.ndb, &txn, &author).labels());
            let settings = Settings::load(ctx.ndb, &txn, &author);
            let home = settings.home_commodity();
            if home != self.home {
                self.home = home.to_owned();
                self.home_generation += 1;
            }
            self.zone = Zone::parse(settings.effective(Setting::Timezone)).unwrap_or(Zone::Local);
            layers.apply(settings.layer_switches());
            let links = crate::boards::links_of(&settings);
            if *self.project_links != *links {
                self.project_links = links.into();
            }
            self.missing_stale = true;
        }
        if self.missing_stale {
            let manifests = manifest::load(ctx.ndb, &txn, &author);
            let missing = sync::missing_locally(&manifests, &self.local);
            self.missing = MissingSegments {
                hashes: missing.iter().map(|m| m.hash).collect(),
                summary: missing_summary(&missing),
            };
            let retired = edits.inputs().map_or(0, |inputs| {
                let here = self.local.iter();
                here.filter(|hash| inputs.retired.contains(hash)).count()
            });
            self.retired_here = if retired == 0 {
                String::new()
            } else {
                format!(
                    "{} here retired by the vault, left out of every total \
                     (`cashflow journal rm <hash>` trashes the file)",
                    segments(retired)
                )
            };
            self.missing_stale = false;
        }
        let Some(inputs) = edits.inputs().cloned() else {
            return fresh;
        };
        let wanted = DerivedWith {
            author: *author.bytes(),
            edits: edits.generation(),
            home: self.home_generation,
            layers: layers.generation(),
        };
        drop(txn);

        // Books that just came back aren't the app's yet; the next job
        // starts next frame, re-deriving from them.
        if self.job.is_some() || fresh.is_some() {
            return fresh;
        }
        let stale = self.derived_with != Some(wanted);
        let due = self.last_scan.is_none_or(|at| at.elapsed() >= RESCAN_EVERY);
        if stale || due {
            let previous = books.map(|books| books.derived.clone());
            let filter = layers.filter();
            self.start_job(ctx, wanted, filter, inputs, previous, stale, since);
        }
        None
    }

    /// Hand the raw ledger to a new job on the host's pool.
    fn start_job(
        &mut self,
        ctx: &mut AppContext<'_>,
        wanted: DerivedWith,
        filter: LayerFilter,
        inputs: Arc<DeriveInputs>,
        previous: Option<Arc<Derived>>,
        derive: bool,
        since: u64,
    ) {
        let job = LoadJob {
            store: self.store.clone(),
            raw: self.raw.take(),
            filter,
            inputs,
            previous,
            home: self.home.clone(),
            derive,
            since,
        };
        self.job = Some(ctx.job_pool.schedule_receivable(move || job.run()));
        self.job_with = Some(wanted);
    }

    /// Take the running job's result if it has finished.
    fn take_job(&mut self) -> Option<Loaded> {
        let done = match self.job.as_mut()?.try_recv() {
            Ok(done) => done,
            Err(oneshot::error::TryRecvError::Empty) => return None,
            Err(oneshot::error::TryRecvError::Closed) => {
                // The pool dropped it (shutting down): try again later.
                self.job = None;
                return None;
            }
        };
        self.job = None;
        let with = self.job_with.take();
        self.last_scan = Some(Instant::now());
        self.raw = done.raw;
        self.error = done.error;
        if self.local != done.local {
            self.local = done.local;
            self.missing_stale = true;
        }
        let loaded = done.loaded?;
        self.derived_with = with;
        self.load_time = Some(done.elapsed);
        Some(loaded)
    }

    /// Take the running sync's report if it has finished.
    fn take_sync(&mut self) {
        let Some(sync) = self.sync.take() else {
            return;
        };
        match sync.try_take() {
            Ok(report) => {
                self.last_sync = Some(sync_outcome(&report));
                if let Ok(transports) = &mut self.transports {
                    let at = chrono::Local::now().format("%H:%M").to_string();
                    for done in &report.transports {
                        let status = transports.iter_mut().find(|t| t.info.name == done.name);
                        if let Some(status) = status {
                            status.last = Some(TransportRun::of(done, &at));
                        }
                    }
                }
                // What arrived is in the journal now: list it straight away.
                self.last_scan = None;
            }
            Err(sync) => self.sync = Some(sync),
        }
    }

    /// Run the configured transports off the UI thread: fetch what the
    /// vault lists that the journal lacks, and offer each remote the local
    /// segments it lacks, as `cashflow sync` does. The admitted segments
    /// come from manifests, so there is nothing to publish afterwards.
    pub fn start_sync(&mut self) {
        if self.sync.is_some() {
            return;
        }
        let previous = std::mem::replace(&mut self.transports, Ok(Vec::new())).unwrap_or_default();
        self.transports = transport_status(&self.dir, previous);
        let transports = Config::load(&self.dir)
            .map_err(|e| e.to_string())
            .and_then(|config| {
                sync::transports(&config, &self.dir, &sync::host(), None).map_err(|e| e.to_string())
            });
        let transports = match transports {
            Ok(transports) => transports,
            Err(err) => {
                self.last_sync = Some(SyncOutcome {
                    failed: true,
                    summary: err,
                });
                return;
            }
        };
        let retired = self
            .edits
            .as_ref()
            .and_then(|c| c.inputs())
            .map(|inputs| inputs.retired.clone())
            .unwrap_or_default();
        let job = SyncJob {
            fetch: self.missing.hashes.clone(),
            push: self
                .local
                .iter()
                .copied()
                .filter(|hash| !retired.contains(hash))
                .collect(),
        };
        let store = self.store.clone();
        let staging = self.dir.staging();
        self.last_sync = None;
        self.sync = Some(Promise::spawn_thread("cashflow-sync", move || {
            sync::run(&transports, &store, &staging, &job, false)
        }));
    }

    /// Where the journal stands, given whether the app has books.
    pub fn state(&self, has_books: bool) -> JournalState {
        if self.error.is_some() {
            return JournalState::Error;
        }
        if !has_books {
            return if self.last_scan.is_some() && self.local.is_empty() {
                JournalState::Empty
            } else {
                JournalState::Loading
            };
        }
        if self.local.is_empty() {
            return JournalState::Empty;
        }
        JournalState::Ready
    }

    /// Whether a job or a sync is running, so the app keeps repainting to
    /// pick up its result.
    pub fn is_busy(&self) -> bool {
        self.job.is_some() || self.sync.is_some()
    }

    /// Why the journal couldn't be listed, in [`JournalState::Error`].
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The segments the vault lists that the journal lacks.
    pub fn missing(&self) -> &MissingSegments {
        &self.missing
    }

    /// One line on the local segments the vault retires, or empty.
    pub fn retired_here(&self) -> &str {
        &self.retired_here
    }

    /// Whether `config.toml` lists a transport to sync with.
    pub fn can_sync(&self) -> bool {
        matches!(&self.transports, Ok(t) if !t.is_empty())
    }

    /// The transports `config.toml` lists and how each last ran, or why
    /// `config.toml` doesn't read.
    pub fn transports(&self) -> Result<&[TransportStatus], &str> {
        match &self.transports {
            Ok(transports) => Ok(transports),
            Err(err) => Err(err),
        }
    }

    /// The vault's home commodity, every amount is converted into.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Whether a sync is running.
    pub fn is_syncing(&self) -> bool {
        self.sync.is_some()
    }

    /// How the last sync went, until the next one starts.
    pub fn last_sync(&self) -> Option<&SyncOutcome> {
        self.last_sync.as_ref()
    }

    /// How long the job that derived the books in hand took.
    pub fn load_time(&self) -> Option<Duration> {
        self.load_time
    }

    /// Where the journal is.
    pub fn journal_path(&self) -> &str {
        &self.journal_path
    }

    /// Which Headway boards each project links, as the vault last had them.
    pub fn project_links(&self) -> &Arc<[ProjectLink]> {
        &self.project_links
    }

    /// The chart's display names and icons, as the vault last had them.
    pub fn labels(&self) -> &Arc<Labels> {
        &self.labels
    }

    /// The synced time zone (the vault's `timezone` setting), as the vault
    /// last had it: what the Adventure dates vault notes in. The machine's
    /// own zone until the vault says otherwise.
    pub fn zone(&self) -> Zone {
        self.zone
    }
}

/// The transports `dir`'s `config.toml` lists, each keeping its last run
/// from `previous` by name, or why `config.toml` doesn't read.
fn transport_status(
    dir: &CashflowDir,
    mut previous: Vec<TransportStatus>,
) -> Result<Vec<TransportStatus>, String> {
    let config = Config::load(dir).map_err(|e| e.to_string())?;
    let transports = sync::describe(&config)
        .into_iter()
        .map(|info| {
            let last = previous
                .iter_mut()
                .find(|p| p.info.name == info.name)
                .and_then(|p| p.last.take());
            TransportStatus { info, last }
        })
        .collect();
    Ok(transports)
}

/// Write the layer flips `layers` queued to the vault of the account whose
/// secret is `secret`. A write that fails is logged and dropped: the
/// switch still holds on this machine, it just doesn't follow the account.
fn write_layers(layers: &mut Layers, secret: &[u8; 32], ctx: &mut AppContext<'_>) {
    let writes = layers.take_writes();
    if writes.is_empty() {
        return;
    }
    let Some(vault) = Vault::new(secret) else {
        tracing::error!("cashflow: no vault to save layer switches in");
        return;
    };
    for write in writes {
        if let Err(err) = cashflow::settings::set_layer(&vault, ctx.ndb, &write.layer, write.on) {
            tracing::error!("cashflow: saving layer {}: {err}", write.layer);
        }
    }
}

/// "3 segments are on bigbox and 1 on laptop", or empty when nothing is
/// missing.
fn missing_summary(missing: &[sync::Missing<'_>]) -> String {
    use std::fmt::Write;

    let mut text = String::new();
    let mut hosts = missing.chunk_by(|a, b| a.manifest.host == b.manifest.host);
    let Some(first) = hosts.next() else {
        return text;
    };
    let _ = write!(
        text,
        "{} on {}",
        segments(first.len()),
        first[0].manifest.host
    );
    for group in hosts {
        let _ = write!(text, ", {} on {}", group.len(), group[0].manifest.host);
    }
    text
}

/// One line on what a sync did.
fn sync_outcome(report: &SyncReport) -> SyncOutcome {
    let errors = report.transports.iter().flat_map(|t| &t.errors);
    let rejected: usize = report
        .transports
        .iter()
        .map(|t| t.admit.rejected.len())
        .sum();
    let failed = report.has_errors() || rejected > 0;
    let mut summary = format!(
        "Synced: fetched {}, pushed {}",
        segments(report.admitted().count()),
        segments(report.transports.iter().map(|t| t.pushed.len()).sum())
    );
    if !report.still_missing.is_empty() {
        summary.push_str(&format!(
            ", {} still missing",
            segments(report.still_missing.len())
        ));
    }
    if rejected > 0 {
        summary.push_str(&format!(", {} rejected", segments(rejected)));
    }
    if let Some(err) = errors.into_iter().next() {
        summary.push_str(&format!(". {err}"));
    }
    SyncOutcome { failed, summary }
}

/// `1 segment` or `n segments`.
fn segments(n: usize) -> String {
    if n == 1 {
        "1 segment".to_owned()
    } else {
        format!("{n} segments")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{Journal, SegmentHeader};

    fn write(store: &JournalStore, imported: u64, id: &str, account: &str) -> SegmentHash {
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported,
            layer: "t".to_owned(),
        };
        let text = format!(
            "2024-01-0{imported} * Shop\n  ; id: {id}\n  {account}  4.50 CAD\n  assets:checking\n"
        );
        store
            .write_segment(&header, &Journal::parse(&text).journal)
            .unwrap()
    }

    fn job(store: &JournalStore, raw: Option<RawLedger>, derive: bool) -> LoadJob {
        LoadJob {
            store: store.clone(),
            raw,
            filter: LayerFilter::All,
            inputs: Arc::new(DeriveInputs::from_edits(cashflow::rules::default_edits())),
            previous: None,
            home: "CAD".to_owned(),
            derive,
            since: 0,
        }
    }

    /// A first job loads everything; a rescan with nothing new derives
    /// nothing; a rescan after a segment arrives merges just that one.
    #[test]
    fn a_rescan_merges_only_new_segments() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let first = write(&store, 1, "t:1", "expenses:food");

        let done = job(&store, None, true).run();
        assert!(done.error.is_none());
        assert_eq!(done.local, [first]);
        let books = done.loaded.expect("a first load derives").books;
        assert_eq!(books.ledger().txns().len(), 1);

        let done = job(&store, done.raw, false).run();
        assert!(done.loaded.is_none(), "nothing new, no derive");

        let second = write(&store, 2, "t:2", "expenses:fun");
        let done = job(&store, done.raw, false).run();
        let books = done.loaded.expect("a new segment derives").books;
        assert_eq!(books.ledger().txns().len(), 2);
        assert_eq!(books.report.loaded, [second], "only the new one is read");
        let mut both = vec![first, second];
        both.sort_unstable();
        assert_eq!(done.local, both);
    }

    /// A raw ledger loaded without a segment the vault now retires is
    /// reloaded, not refreshed, so the retired one drops out.
    #[test]
    fn a_retirement_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let first = write(&store, 1, "t:1", "expenses:food");
        write(&store, 2, "t:2", "expenses:fun");
        let done = job(&store, None, true).run();
        assert_eq!(done.loaded.unwrap().books.ledger().txns().len(), 2);

        let mut retire = job(&store, done.raw, false);
        retire.inputs = Arc::new(DeriveInputs {
            retired: HashSet::from([first]),
            ..(*retire.inputs).clone()
        });
        let books = retire.run().loaded.expect("a reload derives").books;
        assert_eq!(books.ledger().txns().len(), 1);
        assert_eq!(books.report.skipped, [first]);
    }

    /// An empty or absent journal directory loads as no books, not an
    /// error.
    #[test]
    fn an_absent_journal_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let done = job(&store, None, true).run();
        assert!(done.error.is_none());
        assert!(done.local.is_empty());
        assert!(done.loaded.unwrap().books.ledger().txns().is_empty());
    }
}
