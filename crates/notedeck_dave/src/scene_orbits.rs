//! The scene map's satellites: the subagents and child sessions that orbit
//! the session that started them.
//!
//! A Claude subagent has no place of its own on the map: it is a dot in orbit
//! around the session running it, filled while it runs in the foreground and
//! hollow in the background. A child session keeps its own node and is
//! tethered to its parent, and orbits it too as a slightly larger dot, so a
//! parent shows at a glance everything it is waiting on.
//!
//! [`Orbits::sync`] rebuilds the list only when a hash of what it shows
//! changes, or when a finished satellite's linger runs out, so a frame where
//! nothing moved costs one pass over the sessions. A satellite seen finishing
//! stays [`LINGER_SECS`], dimmed, then drops off; one already finished when
//! first seen is history and isn't drawn. Each parent's orbit turns only
//! while one of its satellites runs, from the angle it stopped at.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::hash::{Hash, Hasher};

use egui::{Pos2, Vec2};

use crate::agent_status::AgentStatus;
use crate::session::{EdgeKind, SessionId, SessionManager};

/// How long a finished satellite stays in orbit, dimmed, before it drops
/// off, in seconds.
pub const LINGER_SECS: f64 = 30.0;
/// Seconds an orbit takes to turn once while a satellite runs.
const SECS_PER_TURN: f64 = 24.0;

/// What a satellite stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SatelliteKind {
    /// A subagent the parent waits on: a small filled dot.
    Foreground,
    /// A subagent running in the background: a small hollow dot.
    Background,
    /// A child session (`relation=child`): a larger filled dot, beside its
    /// own node on the map.
    Child,
}

/// One session that may orbit another, as [`Orbits::update`] is given it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    /// The session the satellite opens.
    pub id: SessionId,
    /// The session it orbits.
    pub parent: SessionId,
    pub kind: SatelliteKind,
    pub status: AgentStatus,
}

/// A satellite the map draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Satellite {
    /// The session the satellite opens.
    pub id: SessionId,
    /// The session it orbits.
    pub parent: SessionId,
    pub kind: SatelliteKind,
    pub status: AgentStatus,
    /// When, in egui seconds, it was seen to finish.
    pub finished_at: Option<f64>,
    /// Its place among its parent's satellites, `slot` of `of`.
    slot: usize,
    of: usize,
}

impl Satellite {
    /// Whether it is still working, which keeps its orbit turning.
    pub fn running(&self) -> bool {
        is_running(self.status)
    }

    /// How opaque it is drawn: dimmed once finished.
    pub fn opacity(&self) -> f32 {
        if self.finished_at.is_some() {
            FINISHED_OPACITY
        } else {
            1.0
        }
    }
}

/// A finished satellite's opacity while it lingers.
const FINISHED_OPACITY: f32 = 0.35;

/// Where one parent's orbit has turned to.
struct Spin {
    parent: SessionId,
    /// Radians, added to every satellite's place around the parent.
    angle: f32,
}

/// Every satellite on the map, kept across frames.
#[derive(Default)]
pub struct Orbits {
    /// A hash of the candidates the list was built from.
    key: Option<u64>,
    /// By parent, then kind, then id.
    satellites: Vec<Satellite>,
    spins: Vec<Spin>,
    /// When, in egui seconds, the first lingering satellite drops off.
    expires: Option<f64>,
    /// The egui time of the last [`update`](Self::update).
    last_now: Option<f64>,
}

impl Orbits {
    /// Bring the satellites up to date with `sessions` at egui time `now`:
    /// every subagent's session, and every child session on the lineage.
    pub fn sync(&mut self, sessions: &SessionManager, now: f64) {
        self.update(|| candidates(sessions), now);
    }

    /// [`sync`](Self::sync) from the satellites `candidates` yields, which
    /// is called once to check for a change and again to rebuild on one.
    pub fn update<I>(&mut self, candidates: impl Fn() -> I, now: f64)
    where
        I: Iterator<Item = Candidate>,
    {
        let key = key(candidates());
        let expired = self.expires.is_some_and(|t| now >= t);
        if self.key != Some(key) || expired {
            self.key = Some(key);
            self.rebuild(candidates(), now);
        }
        self.turn(now);
    }

    /// The satellites to draw, by parent.
    pub fn satellites(&self) -> &[Satellite] {
        &self.satellites
    }

    /// The subagent `id`'s satellite, if it is in orbit. A child session
    /// has its own node, so this never finds one.
    pub fn subagent(&self, id: SessionId) -> Option<&Satellite> {
        self.satellites
            .iter()
            .find(|s| s.id == id && s.kind != SatelliteKind::Child)
    }

    /// Where `sat` is drawn at `radius` around its parent at `parent`.
    pub fn position(&self, sat: &Satellite, parent: Pos2, radius: f32) -> Pos2 {
        let spin = self
            .spins
            .iter()
            .find(|s| s.parent == sat.parent)
            .map_or(0.0, |s| s.angle);
        // The first sits at the top; the rest spread evenly round.
        let angle = spin + sat.slot as f32 * TAU / sat.of.max(1) as f32 - FRAC_PI_2;
        parent + Vec2::angled(angle) * radius
    }

    /// Seconds from `now` until a lingering satellite drops off, if one
    /// does.
    pub fn next_expiry(&self, now: f64) -> Option<f64> {
        self.expires.map(|t| (t - now).max(0.0))
    }

    fn rebuild(&mut self, candidates: impl Iterator<Item = Candidate>, now: f64) {
        let old = std::mem::take(&mut self.satellites);
        for c in candidates {
            let finished_at = if is_finished(c.status) {
                let prev = old
                    .iter()
                    .find(|s| s.id == c.id && s.parent == c.parent && s.kind == c.kind);
                // Already finished when first seen: history, not a finish.
                let Some(prev) = prev else {
                    continue;
                };
                let at = prev.finished_at.unwrap_or(now);
                if now - at >= LINGER_SECS {
                    continue;
                }
                Some(at)
            } else {
                None
            };
            self.satellites.push(Satellite {
                id: c.id,
                parent: c.parent,
                kind: c.kind,
                status: c.status,
                finished_at,
                slot: 0,
                of: 0,
            });
        }
        self.satellites.sort_by_key(|s| (s.parent, s.kind, s.id));

        let mut start = 0;
        while start < self.satellites.len() {
            let parent = self.satellites[start].parent;
            let end = start
                + self.satellites[start..]
                    .iter()
                    .take_while(|s| s.parent == parent)
                    .count();
            let of = end - start;
            for (slot, sat) in self.satellites[start..end].iter_mut().enumerate() {
                sat.slot = slot;
                sat.of = of;
            }
            start = end;
        }

        let sats = &self.satellites;
        self.spins
            .retain(|spin| sats.iter().any(|s| s.parent == spin.parent));
        for sat in sats {
            if !self.spins.iter().any(|s| s.parent == sat.parent) {
                self.spins.push(Spin {
                    parent: sat.parent,
                    angle: 0.0,
                });
            }
        }
        self.expires = sats
            .iter()
            .filter_map(|s| s.finished_at)
            .map(|t| t + LINGER_SECS)
            .reduce(f64::min);
    }

    /// Turn each orbit with a running satellite on by the time since the
    /// last update.
    fn turn(&mut self, now: f64) {
        let dt = self.last_now.map_or(0.0, |last| (now - last).max(0.0));
        self.last_now = Some(now);
        if dt == 0.0 {
            return;
        }
        let step = (dt / SECS_PER_TURN) as f32 * TAU;
        for spin in &mut self.spins {
            let turning = self
                .satellites
                .iter()
                .any(|s| s.parent == spin.parent && s.running());
            if turning {
                spin.angle = (spin.angle + step) % TAU;
            }
        }
    }
}

/// Every session that orbits another: each subagent's session around the
/// session running it, and each child session around its spawner.
fn candidates(sessions: &SessionManager) -> impl Iterator<Item = Candidate> + '_ {
    let subagents = sessions.iter().filter_map(|s| {
        let link = s.subagent_of.as_ref()?;
        Some(Candidate {
            id: s.id,
            parent: link.parent,
            kind: if link.background {
                SatelliteKind::Background
            } else {
                SatelliteKind::Foreground
            },
            status: s.status(),
        })
    });
    let children = sessions
        .lineage_edges()
        .iter()
        .filter(|e| e.kind == EdgeKind::Child)
        .filter_map(|e| {
            Some(Candidate {
                id: e.to,
                parent: e.from,
                kind: SatelliteKind::Child,
                status: sessions.get(e.to)?.status(),
            })
        });
    subagents.chain(children)
}

/// An order-independent hash of `candidates`, computed without allocating.
fn key(candidates: impl Iterator<Item = Candidate>) -> u64 {
    candidates
        .map(|c| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            c.id.hash(&mut h);
            c.parent.hash(&mut h);
            c.kind.hash(&mut h);
            c.status.hash(&mut h);
            h.finish()
        })
        .fold(0u64, u64::wrapping_add)
}

fn is_running(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Working | AgentStatus::NeedsInput)
}

fn is_finished(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Done | AgentStatus::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: SessionId, status: AgentStatus) -> Candidate {
        Candidate {
            id,
            parent: 1,
            kind: SatelliteKind::Foreground,
            status,
        }
    }

    fn update(orbits: &mut Orbits, cands: &[Candidate], now: f64) {
        orbits.update(|| cands.iter().copied(), now);
    }

    fn ids(orbits: &Orbits) -> Vec<SessionId> {
        orbits.satellites().iter().map(|s| s.id).collect()
    }

    /// A subagent seen finishing lingers, dimmed, for [`LINGER_SECS`], then
    /// drops off even though nothing else changed.
    #[test]
    fn finished_satellite_lingers_then_drops() {
        let mut o = Orbits::default();
        update(&mut o, &[sub(2, AgentStatus::Working)], 0.0);
        assert_eq!(o.satellites()[0].opacity(), 1.0);

        let done = [sub(2, AgentStatus::Done)];
        update(&mut o, &done, 5.0);
        assert_eq!(o.satellites()[0].finished_at, Some(5.0));
        assert!(o.satellites()[0].opacity() < 1.0);
        assert_eq!(o.next_expiry(5.0), Some(LINGER_SECS));

        update(&mut o, &done, 5.0 + LINGER_SECS - 1.0);
        assert_eq!(ids(&o), [2]);
        update(&mut o, &done, 5.0 + LINGER_SECS);
        assert!(o.satellites().is_empty());
        assert_eq!(o.next_expiry(40.0), None);
    }

    /// A subagent already finished when the map first sees it is history.
    #[test]
    fn already_finished_is_not_drawn() {
        let mut o = Orbits::default();
        update(
            &mut o,
            &[sub(2, AgentStatus::Done), sub(3, AgentStatus::Working)],
            0.0,
        );
        assert_eq!(ids(&o), [3]);
    }

    /// One parent's satellites spread evenly round it, the first on top.
    #[test]
    fn satellites_spread_evenly() {
        let mut o = Orbits::default();
        let cands = [
            sub(2, AgentStatus::Idle),
            sub(3, AgentStatus::Idle),
            Candidate {
                kind: SatelliteKind::Child,
                ..sub(4, AgentStatus::Idle)
            },
        ];
        update(&mut o, &cands, 0.0);
        let at: Vec<Pos2> = o
            .satellites()
            .iter()
            .map(|s| o.position(s, Pos2::ZERO, 10.0))
            .collect();
        assert!((at[0] - Pos2::new(0.0, -10.0)).length() < 1e-4, "{at:?}");
        for (i, a) in at.iter().enumerate() {
            assert!((a.to_vec2().length() - 10.0).abs() < 1e-4);
            let b = at[(i + 1) % at.len()];
            // Neighbours a third of a turn apart: a chord of r·√3.
            assert!(((*a - b).length() - 10.0 * 3f32.sqrt()).abs() < 1e-3);
        }
    }

    /// An orbit turns while one of its satellites runs, and stays where it
    /// stopped once none does.
    #[test]
    fn orbit_turns_only_while_running() {
        let mut o = Orbits::default();
        let running = [sub(2, AgentStatus::Working)];
        update(&mut o, &running, 0.0);
        let start = o.position(&o.satellites()[0], Pos2::ZERO, 10.0);
        update(&mut o, &running, SECS_PER_TURN / 4.0);
        let quarter = o.position(&o.satellites()[0], Pos2::ZERO, 10.0);
        assert!(
            (quarter - Pos2::new(10.0, 0.0)).length() < 1e-3,
            "{quarter:?}"
        );
        assert!((start - quarter).length() > 1.0);

        let idle = [sub(2, AgentStatus::Idle)];
        update(&mut o, &idle, SECS_PER_TURN / 4.0);
        update(&mut o, &idle, SECS_PER_TURN);
        let still = o.position(&o.satellites()[0], Pos2::ZERO, 10.0);
        assert!((still - quarter).length() < 1e-3, "{still:?}");
    }

    /// The same candidates in any order give the same satellites.
    #[test]
    fn order_does_not_matter() {
        let a = [sub(3, AgentStatus::Working), sub(2, AgentStatus::Idle)];
        let b = [a[1], a[0]];
        let (mut x, mut y) = (Orbits::default(), Orbits::default());
        update(&mut x, &a, 0.0);
        update(&mut y, &b, 0.0);
        assert_eq!(x.satellites(), y.satellites());
    }
}
