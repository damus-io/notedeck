//! A running [`Macro`](super::Macro): which step is next and when it may run.
//!
//! A macro navigates, then acts. Its first step usually moves the user
//! somewhere (Dave's `dave_open_issue` opens the session's card in Headway)
//! and the next acts on where it landed (Headway's `headway_open_explainer`).
//! So a step runs only once the one before it has **settled** ([`Settle`]):
//! the open it raised has resolved, the navigation it queued has been
//! applied, the step's app is in front with the tool available, and that
//! app has had a full frame to draw what it landed on. A step that doesn't
//! settle within [`MACRO_TIMEOUT_SECS`] aborts the run.
//!
//! This is the bookkeeping only; the chrome reads the world into a
//! [`Settle`] and dispatches the steps (see `Chrome::advance_macro`).

use std::fmt;

/// How long a step may wait to settle before the run is aborted, in seconds
/// of egui input time.
pub const MACRO_TIMEOUT_SECS: f64 = 3.0;

/// How often a waiting run asks for a frame, so it re-checks without input
/// and without spinning the render loop.
pub const MACRO_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// The one macro running, owned by the chrome.
#[derive(Debug)]
pub struct MacroRun {
    /// The macro's name in the keymap, looked up again each frame, so a
    /// keymap replaced mid-run can't run steps from the old one.
    name: String,
    /// The index of the step to run next.
    next: usize,
    /// The egui time the last step ran at; its wait times out from here.
    last_dispatch: f64,
    /// Frames ended with the world settled since the last step ran. The next
    /// step runs only once a frame has rendered with it settled, so the app a
    /// step navigated to has drawn (and seeded) the route it landed on.
    settled_frames: u32,
}

/// What a waiting step's world looks like at the end of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settle {
    /// An open the last step raised is still being retried
    /// (the chrome's `PendingOpen`).
    pub open_pending: bool,
    /// Navigation requests are queued that the chrome hasn't applied yet.
    pub nav_queued: bool,
    /// The app that owns the next step's tool is the one in front (always so
    /// for the chrome's own tools and data tools).
    pub owner_active: bool,
    /// The next step's tool would do something now.
    pub available: bool,
}

/// The first condition of [`Settle`] that doesn't hold, or that the frame
/// after the settle hasn't rendered yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsettled {
    /// An open is still resolving.
    OpenPending,
    /// Navigation is still queued.
    NavQueued,
    /// The tool's app isn't in front.
    OwnerInactive,
    /// The tool isn't available.
    Unavailable,
    /// Settled, but no frame has rendered with it yet.
    NotRendered,
}

impl fmt::Display for Unsettled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OpenPending => "the open it waits on never resolved",
            Self::NavQueued => "navigation stayed queued",
            Self::OwnerInactive => "its app never came to the front",
            Self::Unavailable => "its tool never became available",
            Self::NotRendered => "its app never rendered",
        })
    }
}

impl Settle {
    /// The first condition that doesn't hold, in the order the world settles:
    /// the open resolves, its navigation applies, the app comes to the front,
    /// the tool becomes available.
    pub fn unmet(&self) -> Option<Unsettled> {
        if self.open_pending {
            Some(Unsettled::OpenPending)
        } else if self.nav_queued {
            Some(Unsettled::NavQueued)
        } else if !self.owner_active {
            Some(Unsettled::OwnerInactive)
        } else if !self.available {
            Some(Unsettled::Unavailable)
        } else {
            None
        }
    }
}

/// What a waiting run should do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Not yet; ask for another frame.
    Wait,
    /// Run the step at this index, then report it with
    /// [`MacroRun::dispatched`].
    Run(usize),
    /// Give up: the step at this index didn't settle in time, for this
    /// reason.
    Abort(usize, Unsettled),
}

impl MacroRun {
    /// A run of the macro `name`, its first step about to run at `now`.
    pub fn new(name: String, now: f64) -> Self {
        Self {
            name,
            next: 0,
            last_dispatch: now,
            settled_frames: 0,
        }
    }

    /// The macro's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The index of the step to run next.
    pub fn next_step(&self) -> usize {
        self.next
    }

    /// Record that the next step ran at `now`. Returns whether the macro,
    /// `steps` long, has more steps to run.
    pub fn dispatched(&mut self, now: f64, steps: usize) -> bool {
        self.next += 1;
        self.last_dispatch = now;
        self.settled_frames = 0;
        self.next < steps
    }

    /// Decide, at the end of a frame at `now`, whether the next step may run,
    /// given the world as `settle` describes it. A step runs on the second
    /// frame end in a row that finds the world settled — the frame between
    /// them rendered it — and aborts once [`MACRO_TIMEOUT_SECS`] have passed
    /// since the last step without that.
    pub fn poll(&mut self, now: f64, settle: Settle) -> Advance {
        let unmet = match settle.unmet() {
            Some(unmet) => {
                self.settled_frames = 0;
                unmet
            }
            None if self.settled_frames > 0 => return Advance::Run(self.next),
            None => {
                self.settled_frames = 1;
                Unsettled::NotRendered
            }
        };
        if now - self.last_dispatch >= MACRO_TIMEOUT_SECS {
            return Advance::Abort(self.next, unmet);
        }
        Advance::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The world with every condition met.
    const SETTLED: Settle = Settle {
        open_pending: false,
        nav_queued: false,
        owner_active: true,
        available: true,
    };

    /// A run whose first step ran at 0.0, now waiting on its second.
    fn waiting() -> MacroRun {
        let mut run = MacroRun::new("m".to_owned(), 0.0);
        assert!(run.dispatched(0.0, 2));
        run
    }

    /// Each settle rule holds the step back on its own, and names itself as
    /// the reason, earliest first.
    #[test]
    fn each_rule_holds_the_step_back() {
        let cases = [
            (
                Settle {
                    open_pending: true,
                    ..SETTLED
                },
                Unsettled::OpenPending,
            ),
            (
                Settle {
                    nav_queued: true,
                    ..SETTLED
                },
                Unsettled::NavQueued,
            ),
            (
                Settle {
                    owner_active: false,
                    ..SETTLED
                },
                Unsettled::OwnerInactive,
            ),
            (
                Settle {
                    available: false,
                    ..SETTLED
                },
                Unsettled::Unavailable,
            ),
        ];
        for (settle, reason) in cases {
            assert_eq!(settle.unmet(), Some(reason));
            let mut run = waiting();
            for frame in 1..100 {
                assert_eq!(run.poll(f64::from(frame) / 60.0, settle), Advance::Wait);
            }
            assert_eq!(
                run.poll(MACRO_TIMEOUT_SECS, settle),
                Advance::Abort(1, reason)
            );
        }
        let all = Settle {
            open_pending: true,
            nav_queued: true,
            owner_active: false,
            available: false,
        };
        assert_eq!(all.unmet(), Some(Unsettled::OpenPending), "earliest first");
        assert_eq!(SETTLED.unmet(), None);
    }

    /// Settled, the step waits one frame for the app to render what it
    /// landed on, then runs.
    #[test]
    fn a_settled_step_runs_after_one_rendered_frame() {
        let mut run = waiting();
        assert_eq!(run.poll(0.1, SETTLED), Advance::Wait);
        assert_eq!(run.poll(0.2, SETTLED), Advance::Run(1));
        assert!(!run.dispatched(0.2, 2), "that was the last step");
    }

    /// A settle that comes undone starts the rendered-frame count over.
    #[test]
    fn an_unsettle_starts_the_frame_count_over() {
        let mut run = waiting();
        let unavailable = Settle {
            available: false,
            ..SETTLED
        };
        assert_eq!(run.poll(0.1, SETTLED), Advance::Wait);
        assert_eq!(run.poll(0.2, unavailable), Advance::Wait);
        assert_eq!(run.poll(0.3, SETTLED), Advance::Wait);
        assert_eq!(run.poll(0.4, SETTLED), Advance::Run(1));
    }

    /// Settled but not yet rendered when the time runs out says so.
    #[test]
    fn a_timeout_on_the_render_frame_names_it() {
        let mut run = waiting();
        assert_eq!(
            run.poll(MACRO_TIMEOUT_SECS + 1.0, SETTLED),
            Advance::Abort(1, Unsettled::NotRendered)
        );
    }

    /// The timeout counts from the last step, not the start of the run.
    #[test]
    fn the_timeout_counts_from_the_last_step() {
        let mut run = MacroRun::new("m".to_owned(), 0.0);
        assert!(run.dispatched(0.0, 3));
        assert_eq!(run.poll(2.0, SETTLED), Advance::Wait);
        assert_eq!(run.poll(2.5, SETTLED), Advance::Run(1));
        assert!(run.dispatched(2.5, 3));
        let unavailable = Settle {
            available: false,
            ..SETTLED
        };
        assert_eq!(
            run.poll(5.0, unavailable),
            Advance::Wait,
            "2.5s since step 1"
        );
        assert_eq!(
            run.poll(5.5, unavailable),
            Advance::Abort(2, Unsettled::Unavailable)
        );
    }
}
