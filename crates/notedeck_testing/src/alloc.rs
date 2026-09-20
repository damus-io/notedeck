//! Measuring what one frame of a Notedeck app allocates.
//!
//! [`crate::stepping::step_device_frames`] and [`DeviceHarness::step`] are the
//! per-frame boundary in this harness, and this module is what wraps a tape
//! measure around them. It provides the pieces; the assertion lives in the
//! integration test that owns the `#[global_allocator]`, because a global
//! allocator is process-wide and one per binary, and this crate is a library
//! linked into many test binaries.
//!
//! [`DeviceHarness::step`]: crate::device::DeviceHarness
//!
//! # A ratchet, not a zero
//!
//! Notedeck cannot assert that a frame allocates nothing, and probably never
//! will. It is an egui/eframe app, and egui rebuilds its shape list from
//! scratch every pass by design; notedeck's own UI code allocates freely on top
//! of that. "Assert zero" would fail on its first run and teach nobody
//! anything.
//!
//! So the shape here is [`AllocBudget`]: a number that was measured, written
//! down with the conditions it was measured under, and checked in both
//! directions.
//!
//! - Going **over** is a regression, and catching it is the whole point.
//! - Going **under** without the number being updated is also a failure, on
//!   purpose. A ceiling nobody has looked at in a year stops being a
//!   measurement and becomes a permission slip. This matches
//!   [`notedeck::media::budget`]'s habit of writing the observed figure into
//!   the constant rather than picking a round number.
//!
//! # Why the counters are thread-local
//!
//! Notedeck is heavily multi-threaded: relay sockets, nostrdb ingest, image
//! decode on the [`notedeck::JobPool`], and a tokio runtime. A global
//! `AtomicU64` would measure whatever the whole process did while the frame
//! happened to be running, which is a property of the scheduler and not of the
//! frame. Per-thread counters on the thread that drives `step()` are the only
//! measurement that holds still.
//!
//! **The loophole this leaves is real and is not a bug to be fixed here:** an
//! allocation "removed" by moving it to a background thread disappears from
//! this measurement and looks like an improvement. That is sometimes a genuine
//! improvement — the render loop must not block, so moving work off it is
//! usually right — and sometimes it is the same allocation wearing a hat.
//! [`AllocCounts::process`] is what keeps it visible: every counter is kept
//! twice, once per thread and once process-wide, and the process-wide figure is
//! reported but never asserted on, because it is too noisy to assert on and too
//! informative to drop.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

// -- the allocator ---------------------------------------------------------

/// The system allocator, with a tally kept per thread and another kept for the
/// whole process.
///
/// Install it in the integration test that wants to measure, not here:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: notedeck_testing::alloc::CountingAllocator =
///     notedeck_testing::alloc::CountingAllocator;
/// ```
///
/// A `#[global_allocator]` is process-wide and one per binary, so it sees the
/// test harness's own traffic as well as the frame's. Only the difference
/// across a measured call means anything, which is what [`measure`] returns.
pub struct CountingAllocator;

// SAFETY: every method forwards to `System` with the pointer and layout it was
// handed, unchanged. The counting is a side effect on a `Cell` this thread owns
// plus relaxed atomic adds, and touches no allocator state.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(|c| {
            c.allocs += 1;
            c.bytes += layout.size() as u64;
        });
        record_site(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(|c| {
            c.allocs += 1;
            c.bytes += layout.size() as u64;
        });
        record_site(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted apart from a fresh allocation, because it is a different bug.
        // "This frame built something" and "this frame grew something past the
        // capacity it declared" both matter, but they send you looking in
        // different places — the second is usually a `Vec` in a `*_ui` function
        // that should have been hoisted into state.
        record(|c| {
            c.reallocs += 1;
            c.bytes += new_size.saturating_sub(layout.size()) as u64;
        });
        record_site(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(|c| c.frees += 1);
        unsafe { System.dealloc(ptr, layout) }
    }
}

thread_local! {
    /// `const`-initialized and `Copy`, so this has no destructor and no lazy
    /// setup: the access compiles to a plain thread-local load. That matters
    /// more than it looks — a thread-local that initialized itself lazily could
    /// allocate, from inside the allocator, on the first allocation.
    static COUNTS: Cell<Counters> = const { Cell::new(Counters::ZERO) };
}

/// Process-wide tallies, in the same order as [`Counters`]'s fields.
///
/// Relaxed ordering throughout: these are never used to synchronize anything,
/// only reported, and a measurement that added a fence to every allocation in
/// the process would change what it was measuring.
static PROCESS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Update this thread's tally and the process's.
///
/// Does nothing to the thread-local if it has already been torn down, which
/// happens while a thread is exiting and is never inside a measurement.
fn record(f: impl FnOnce(&mut Counters)) {
    let mut delta = Counters::ZERO;
    f(&mut delta);

    let _ = COUNTS.try_with(|cell| {
        let mut counts = cell.get();
        counts.allocs += delta.allocs;
        counts.reallocs += delta.reallocs;
        counts.frees += delta.frees;
        counts.bytes += delta.bytes;
        cell.set(counts);
    });

    for (slot, value) in
        PROCESS
            .iter()
            .zip([delta.allocs, delta.reallocs, delta.frees, delta.bytes])
    {
        if value != 0 {
            slot.fetch_add(value, Ordering::Relaxed);
        }
    }
}

/// This thread's tally so far.
fn read_thread() -> Counters {
    COUNTS.try_with(Cell::get).unwrap_or(Counters::ZERO)
}

/// Every thread's tally so far.
fn read_process() -> Counters {
    let [allocs, reallocs, frees, bytes] =
        std::array::from_fn(|i| PROCESS[i].load(Ordering::Relaxed));
    Counters {
        allocs,
        reallocs,
        frees,
        bytes,
    }
}

// -- what it counts --------------------------------------------------------

/// What went through the allocator, on one thread or across the process.
///
/// Four numbers rather than one, because "something allocated" is the least
/// useful true thing this could say. Which kind of traffic it was is most of
/// the diagnosis.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Fresh allocations, zeroed or not.
    pub allocs: u64,
    /// Grow-or-shrink requests: a container that outgrew its declared capacity.
    pub reallocs: u64,
    /// Frees. Counted because a frame that allocates and frees before it
    /// returns is still a frame that allocated, and a net figure would call
    /// that clean — which for an immediate-mode UI would call *everything*
    /// clean, since almost all of it is freed by the end of the pass.
    pub frees: u64,
    /// Bytes asked for, counting only the growth on a realloc.
    pub bytes: u64,
}

impl Counters {
    const ZERO: Self = Self {
        allocs: 0,
        reallocs: 0,
        frees: 0,
        bytes: 0,
    };

    /// What happened between an earlier reading and this one.
    fn since(self, earlier: Self) -> Self {
        Self {
            allocs: self.allocs.saturating_sub(earlier.allocs),
            reallocs: self.reallocs.saturating_sub(earlier.reallocs),
            frees: self.frees.saturating_sub(earlier.frees),
            bytes: self.bytes.saturating_sub(earlier.bytes),
        }
    }
}

impl fmt::Display for Counters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "allocs {}, reallocs {}, frees {}, bytes {}",
            self.allocs, self.reallocs, self.frees, self.bytes
        )
    }
}

/// One measured region, seen from both the measuring thread and the process.
///
/// The two are kept side by side on purpose. See the module docs: the
/// thread-local figure is the one worth asserting on and the one an allocation
/// can escape by moving to a background thread, and the process figure is what
/// makes that escape visible.
#[derive(Clone, Copy, Default)]
pub struct AllocCounts {
    /// Traffic on the thread that ran the measured code. This is the
    /// measurement.
    pub thread: Counters,
    /// Traffic across every thread, including the one above. Reported, never
    /// asserted on: relay sockets, nostrdb ingest and image decode all land
    /// here, and what they were doing during any particular frame is a property
    /// of the scheduler.
    pub process: Counters,
}

impl fmt::Display for AllocCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ui thread: {} | whole process: {}",
            self.thread, self.process
        )
    }
}

/// Run `f` and report what it allocated.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, AllocCounts) {
    let thread_before = read_thread();
    let process_before = read_process();
    let value = f();
    let counts = AllocCounts {
        thread: read_thread().since(thread_before),
        process: read_process().since(process_before),
    };
    (value, counts)
}

// -- attribution -----------------------------------------------------------

/// How many call sites one measured region will record before it stops.
///
/// High enough to be a census of the handful of frames attribution is ever
/// pointed at, rather than a sample: a truncated ranking is a ranking of
/// whatever ran first, which for an immediate-mode UI is whatever happens to be
/// at the top of the widget tree. The cap exists only so that pointing it at a
/// long run cannot exhaust memory.
const MAX_RECORDED_SITES: usize = 262_144;

thread_local! {
    /// Whether this thread is currently recording call sites.
    ///
    /// Doubles as the re-entrancy guard: capturing a backtrace allocates, so
    /// the flag is cleared for the duration of the capture and the allocations
    /// the capture itself makes fall straight through.
    static RECORDING: Cell<bool> = const { Cell::new(false) };

    /// Sites recorded so far, or `None` when nothing is recording.
    ///
    /// Unsymbolized: [`Backtrace::force_capture`] walks the stack now and
    /// resolves names later, which is what makes this affordable inside the
    /// allocator.
    static SITES: std::cell::RefCell<Option<Vec<(std::backtrace::Backtrace, usize)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Record the stack that asked for `bytes`, if this thread is recording.
///
/// [`Backtrace::force_capture`] rather than `capture`, which is a no-op unless
/// `RUST_BACKTRACE` is set in the environment — attribution that silently
/// returned nothing depending on a shell variable is worse than no attribution.
///
/// [`Backtrace::force_capture`]: std::backtrace::Backtrace::force_capture
fn record_site(bytes: usize) {
    let _ = RECORDING.try_with(|recording| {
        if !recording.get() {
            return;
        }

        recording.set(false);
        let _ = SITES.try_with(|sites| {
            // `try_borrow_mut` rather than `borrow_mut`: the guard above should
            // make a re-entrant borrow impossible, and a panic from inside the
            // global allocator is not how we would want to find out otherwise.
            if let Ok(mut sites) = sites.try_borrow_mut() {
                if let Some(sites) = sites.as_mut() {
                    if sites.len() < MAX_RECORDED_SITES {
                        sites.push((std::backtrace::Backtrace::force_capture(), bytes));
                    }
                }
            }
        });
        recording.set(true);
    });
}

/// One call site, with everything it allocated in the measured region.
pub struct Site {
    /// The most specific frame we could name: the first one that belongs to a
    /// workspace or UI crate rather than to the allocator or to `alloc`/`core`.
    pub frame: String,
    /// Allocations attributed to it.
    pub allocs: u64,
    /// Bytes attributed to it.
    pub bytes: u64,
}

/// Run `f` with call-site recording on, and return the sites it saw, ranked by
/// allocation count.
///
/// Separate from [`measure`] and far more expensive than it: this is the tool
/// you reach for once a budget has already failed, or when writing down where a
/// frame's allocations actually go. It is not on the path of the check itself.
pub fn attribute<T>(f: impl FnOnce() -> T) -> (T, Vec<Site>) {
    SITES.with(|sites| *sites.borrow_mut() = Some(Vec::with_capacity(MAX_RECORDED_SITES)));
    RECORDING.with(|recording| recording.set(true));

    let value = f();

    RECORDING.with(|recording| recording.set(false));
    let captured = SITES
        .with(|sites| sites.borrow_mut().take())
        .unwrap_or_default();

    (value, rank_sites(captured))
}

/// Symbolize and group captured stacks, heaviest first.
fn rank_sites(captured: Vec<(std::backtrace::Backtrace, usize)>) -> Vec<Site> {
    let mut by_frame: std::collections::HashMap<String, (u64, u64)> = Default::default();

    for (backtrace, bytes) in captured {
        let entry = by_frame.entry(blame(&backtrace)).or_default();
        entry.0 += 1;
        entry.1 += bytes as u64;
    }

    let mut sites: Vec<Site> = by_frame
        .into_iter()
        .map(|(frame, (allocs, bytes))| Site {
            frame,
            allocs,
            bytes,
        })
        .collect();

    sites.sort_unstable_by(|a, b| b.allocs.cmp(&a.allocs).then(b.bytes.cmp(&a.bytes)));
    sites
}

/// How many frames of context a site is named with.
///
/// One frame is not enough, and finding that out is most of what this tool is
/// for. The single heaviest site in a Columns frame symbolizes as
/// `egui::ui::Ui::style_mut`, which reads as egui's problem and is not: three
/// frames name it `Ui::style_mut <- Ui::spacing_mut <- NoteView::note_header`,
/// which is a line of notedeck's own code and a thing somebody can go and fix.
///
/// The cost is that one underlying site splits across rows when it has several
/// callers. For a ranking that is the right trade — the callers are what you
/// would have had to go and find anyway.
const BLAME_DEPTH: usize = 3;

/// Name the code worth blaming for a stack.
///
/// The top of every captured stack is this module and the `alloc`/`core`
/// machinery that led into it, which is identical for every allocation and
/// names nothing. Walk down past that to the first [`BLAME_DEPTH`] frames that
/// belong to code somebody here could change.
fn blame(backtrace: &std::backtrace::Backtrace) -> String {
    /// Substrings of frames that are always present and never the answer: the
    /// recorder itself, the backtrace machinery it uses, and the `alloc`/`core`
    /// plumbing that sits between any caller and `__rust_alloc`. Matched as
    /// substrings rather than prefixes because generic frames wrap the
    /// interesting name in angle brackets and turbofish, so `starts_with`
    /// matches almost nothing.
    const NOISE: &[&str] = &[
        "notedeck_testing::alloc",
        "std::backtrace",
        "backtrace::",
        "std::thread::local",
        "__rust_alloc",
        "__rg_alloc",
        "alloc::alloc",
        "alloc::raw_vec",
        "alloc::vec::Vec",
        "alloc::string",
        "alloc::slice",
        "alloc::boxed",
        "alloc::sync::Arc",
        "alloc::rc::Rc",
        "alloc::fmt",
        "alloc::collections",
        "core::fmt",
        "core::ptr::drop_in_place",
        "hashbrown",
        "std::sys",
        "std::io",
    ];

    let mut picked: Vec<String> = Vec::with_capacity(BLAME_DEPTH);
    let rendered = format!("{backtrace}");
    for line in rendered.lines() {
        // Frame lines look like `   3: some::path::function`; the `at
        // file:line` lines that follow are indented further and carry no
        // symbol we can match on.
        let Some((_, symbol)) = line.trim_start().split_once(": ") else {
            continue;
        };
        let symbol = symbol.trim();
        if symbol.is_empty() || NOISE.iter().any(|noise| symbol.contains(noise)) {
            continue;
        }
        picked.push(symbol.to_owned());
        if picked.len() == BLAME_DEPTH {
            break;
        }
    }

    if picked.is_empty() {
        return "<unattributed>".to_owned();
    }
    picked.join("  <-  ")
}

// -- the ratchet -----------------------------------------------------------

use crate::device::DeviceHarness;

/// What one app's steady-state frame is allowed to allocate.
///
/// The two `measured_` numbers are exactly that: what the profile reported,
/// written down. Not rounded, not chosen — a round number nobody measured is a
/// guess wearing a constant's clothes.
/// [`notedeck::media::budget::DEFAULT_TEXTURE_BUDGET`] is written from an
/// observed figure for the same reason.
///
/// The tolerance is a separate field rather than baked into the numbers, so
/// that the measurement stays readable as a measurement and the judgement call
/// stays readable as a judgement call.
#[derive(Clone, Copy)]
pub struct AllocBudget {
    /// The **median** frame's measured allocation count.
    ///
    /// The median rather than the mean, which one pathological frame drags
    /// around, and rather than the max, which is the noisiest thing a frame
    /// window produces. A change that makes the steady-state frame cheaper or
    /// dearer moves the median; scheduling noise does not.
    pub measured_median: u64,

    /// The **worst** frame's measured allocation count.
    ///
    /// Carried separately because the median cannot see a frame that allocates
    /// once every thirty — a cache sweep, a timer, the first frame after some
    /// state changed — and averaging that away is exactly how a check stops
    /// checking.
    pub measured_peak: u64,

    /// How many allocations above the measured figure are still not a
    /// regression.
    ///
    /// A count, not a percentage, and that is the whole point. Percentages are
    /// the wrong unit here, and finding that out is what the tolerance is sized
    /// from:
    ///
    /// - **What drifts innocently is a couple of allocations.** The same frame
    ///   measures 991 on one x86-64 Linux box and 990 on an ubuntu-22.04
    ///   container, bit-exact on each. Different environment, same code, one
    ///   allocation apart.
    /// - **What a regression costs is at least the number of items on screen.**
    ///   An allocation added to a per-note function is multiplied by every note
    ///   drawn — a `format!` in `actionbar_ui` costs seven allocations a frame
    ///   with seven notes visible, and more on a taller window.
    ///
    /// So there is a gap between the two, and an absolute tolerance sits in it.
    /// A percentage does not: 2% of 991 is 19 allocations, which swallows that
    /// `format!` whole, and it would grow as the frame gets more expensive —
    /// exactly backwards, since a dearer frame is one that needs *more*
    /// scrutiny, not more slack.
    ///
    /// What this does not catch is one allocation added to something that runs
    /// once a frame. That is the price, it is about 60 allocations a second
    /// against a baseline of 60,000, and it buys a check that does not go red
    /// because somebody ran it on a different distribution.
    pub tolerance_allocs: u64,
}

impl AllocBudget {
    /// The median count at or under which the frame is unchanged.
    pub fn median_ceiling(&self) -> u64 {
        self.measured_median + self.tolerance_allocs
    }

    /// The peak count at or under which the frame is unchanged.
    pub fn peak_ceiling(&self) -> u64 {
        self.measured_peak + self.tolerance_allocs
    }

    /// The median count below which the budget is stale rather than met.
    ///
    /// Not exact, unlike the ceiling, and the asymmetry is deliberate. A
    /// regression of one allocation per note is worth a failure because it
    /// compounds — seven notes on screen made that one `format!` into seven.
    /// An *improvement* of one allocation is not worth failing somebody's
    /// unrelated pull request over. So: notice every increase, and ask for the
    /// number to be rewritten only when the frame has genuinely got cheaper.
    pub fn median_floor(&self) -> u64 {
        low_water(self.measured_median)
    }
}

/// The level below which a budget is stale rather than met, as a fraction of
/// the measured figure.
///
/// Same shape and the same fraction as
/// [`notedeck::media::budget::low_water`], for a related reason: a budget wants
/// headroom on both sides. Above it is a regression. More than an eighth below
/// it means the frame really did get cheaper and nobody wrote the new number
/// down, so the constant has quietly stopped describing anything.
pub fn low_water(budget: u64) -> u64 {
    budget / 8 * 7
}

/// One frame's worth of measurement, kept per frame rather than summed.
///
/// Per frame and not per run, so a failure can name the frame and so that
/// something allocating every Nth frame cannot average itself away.
pub struct FrameProfile {
    /// What was being driven, for the report and the failure message.
    label: &'static str,
    /// Frames discarded before measurement began.
    warmup: usize,
    /// One entry per measured frame, in order.
    frames: Vec<AllocCounts>,
}

impl FrameProfile {
    /// Drive `device` and record what each frame allocated.
    ///
    /// `warmup` frames run first and are thrown away. They are not a
    /// formality: a freshly booted host spends its first frames building
    /// layouts, resolving fonts, realizing subscriptions and filling caches,
    /// and none of that is steady state. Measuring it would produce a baseline
    /// that describes startup and a check that passes no matter what the app
    /// does afterwards.
    pub fn measure(
        label: &'static str,
        device: &mut DeviceHarness,
        warmup: usize,
        frames: usize,
    ) -> Self {
        for _ in 0..warmup {
            device.step();
        }

        let mut measured = Vec::with_capacity(frames);
        for _ in 0..frames {
            let (_, counts) = measure(|| device.step());
            measured.push(counts);
        }

        Self {
            label,
            warmup,
            frames: measured,
        }
    }

    /// The measured frames, in order.
    pub fn frames(&self) -> &[AllocCounts] {
        &self.frames
    }

    /// Allocation counts on the measured thread, sorted.
    fn sorted_allocs(&self) -> Vec<u64> {
        let mut allocs: Vec<u64> = self.frames.iter().map(|f| f.thread.allocs).collect();
        allocs.sort_unstable();
        allocs
    }

    /// The middle frame's allocation count. See [`AllocBudget::measured_median`].
    pub fn median_allocs(&self) -> u64 {
        let allocs = self.sorted_allocs();
        allocs.get(allocs.len() / 2).copied().unwrap_or(0)
    }

    /// The worst frame's allocation count.
    pub fn peak_allocs(&self) -> u64 {
        self.frames
            .iter()
            .map(|f| f.thread.allocs)
            .max()
            .unwrap_or(0)
    }

    /// The index, within the measured window, of the frame that allocated most.
    fn peak_frame(&self) -> usize {
        self.frames
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| f.thread.allocs)
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// The whole distribution, written out.
    ///
    /// Printed by the check on every run, pass or fail. A ratchet whose number
    /// is only visible when it breaks is a number nobody has looked at.
    pub fn report(&self) -> String {
        let allocs = self.sorted_allocs();
        let (min, median, max) = (
            allocs.first().copied().unwrap_or(0),
            self.median_allocs(),
            allocs.last().copied().unwrap_or(0),
        );

        let bytes: u64 = self.frames.iter().map(|f| f.thread.bytes).sum();
        let reallocs: u64 = self.frames.iter().map(|f| f.thread.reallocs).sum();
        let n = self.frames.len().max(1) as u64;

        let process: u64 = self.frames.iter().map(|f| f.process.allocs).sum();

        format!(
            "{label}: {n} frames measured after {warmup} warm-up frames\n\
             \x20 ui thread   allocs/frame  min {min}  median {median}  max {max} (frame {peak})\n\
             \x20 ui thread   bytes/frame   {mean_bytes} mean, {reallocs} reallocs total\n\
             \x20 all threads allocs/frame  {mean_process} mean (reported only, never asserted \
             on — see notedeck_testing::alloc)",
            label = self.label,
            warmup = self.warmup,
            peak = self.peak_frame(),
            mean_bytes = bytes / n,
            mean_process = process / n,
        )
    }

    /// Check the profile against its written-down budget.
    ///
    /// Fails in both directions. See [`AllocBudget`] for why the ceiling is two
    /// numbers, and [`AllocBudget::median_floor`] for why falling under one is
    /// also a failure.
    #[track_caller]
    pub fn assert_within(&self, budget: AllocBudget) {
        let report = self.report();

        let Some(verdict) = self.verdict(budget) else {
            // Printed on a pass too, not only on a failure. A ratchet whose
            // number is visible only when it breaks is a number nobody has
            // looked at, which is the state the rule was in before this
            // existed. On a failure the panic carries it instead, so it is not
            // printed twice.
            eprintln!("{report}");
            return;
        };

        panic!("{report}\n\n{verdict}\n\n{HOW_TO_FIND_IT}");
    }

    /// Why the profile misses its budget, or `None` if it does not.
    ///
    /// Split out from [`assert_within`](Self::assert_within) so the three
    /// failures are visibly the same shape, and so a caller that wants the
    /// judgement without the panic can have it.
    pub fn verdict(&self, budget: AllocBudget) -> Option<String> {
        let median = self.median_allocs();
        let peak = self.peak_allocs();

        if median > budget.median_ceiling() {
            return Some(format!(
                "The steady-state frame got more expensive. The median frame made {median} \
                 allocations against a measured budget of {} (plus {} allocations of \
                 tolerance), so {} more per frame than when the budget was written down.",
                budget.measured_median,
                budget.tolerance_allocs,
                median - budget.measured_median,
            ));
        }

        if peak > budget.peak_ceiling() {
            return Some(format!(
                "One frame spiked. Frame {} of the measured window made {peak} allocations \
                 against a peak budget of {}. The median is within budget, so this is something \
                 that happens on some frames and not others — a cache sweep, a timer, an \
                 animation, or the first frame after some state changed. Per-frame counters \
                 exist precisely so this cannot average itself away inside an aggregate.",
                self.peak_frame(),
                budget.measured_peak,
            ));
        }

        if median < budget.median_floor() {
            return Some(format!(
                "The frame got cheaper and nobody updated the budget. The median frame made \
                 {median} allocations against a measured {}, which is below the {} floor.\n\n\
                 This is a real improvement and the failure is deliberate. A ceiling nothing \
                 has approached in a year has stopped being a measurement and become a \
                 permission slip. Set measured_median to {median} and measured_peak to {peak}, \
                 say in the commit what made it cheaper, and the ratchet holds the new ground.",
                budget.measured_median,
                budget.median_floor(),
            ));
        }

        None
    }
}

/// Appended to every budget failure. Named here rather than inlined three times
/// so the three failures give the same advice.
const HOW_TO_FIND_IT: &str = "\
To find the call sites: wrap the same frames in `notedeck_testing::alloc::attribute` \
instead of `FrameProfile::measure`. It captures a backtrace per allocation and returns \
the sites ranked by count and bytes, which is how the numbers in this budget were \
arrived at in the first place. It is far slower than the check, so it is a tool you \
reach for once, not something the check runs.

What to look for, in the order they are usually worth looking:

  - A `Vec`, `String` or `format!` built inside a `*_ui` function. CLAUDE.md's \
\"No allocation in ui functions\" is about exactly this: every `*_ui` function runs \
every frame, so a collection built in one is built every frame. Iterate lazily, borrow \
with `Cow`/`&str`, or hoist the allocation into state that is reseeded only when the \
underlying data changes.
  - A `to_string`/`to_owned` on a key or a label that could have stayed a `&str`.
  - An `.collect()` feeding something that only iterates over the result.
  - A realloc, which is a container that outgrew the capacity it was built with.

What is *not* worth looking at: frames inside `egui`, `epaint` and `emath`. egui \
rebuilds its shape list and its layout from scratch every pass by design, and that \
allocation is not ours to remove. It is inside the budget because it is inside the \
frame, not because anyone intends to get rid of it.";
