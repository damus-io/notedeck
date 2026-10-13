//! Where the scene map draws each session: hosts, projects and worktrees as
//! nested circles placed by circle packing, sessions as points inside them.
//!
//! Pure and egui-free apart from [`Pos2`]. [`SceneTree::new`] sorts what it
//! is given by stable keys, so the same sessions always give the same map;
//! [`SceneLayout::pack`] lays the tree out bottom-up.
//!
//! The packing is a port of d3-hierarchy's `packSiblings` (front-chain
//! placement, Wang et al. 2006) and `packEnclose` (Welzl's smallest enclosing
//! circle), <https://github.com/d3/d3-hierarchy>, Copyright 2010-2021 Mike
//! Bostock, ISC license.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use egui::Pos2;

use crate::session::{ChatSession, SessionId, SessionManager, Workspace};

/// Radius of a session's circle: room for a 15 px status ring and the
/// satellites that orbit it.
pub const LEAF_RADIUS: f32 = 22.0;
/// How far a subagent sits from the center of the session running it.
pub const SATELLITE_ORBIT: f32 = 20.0;

/// Gap between hosts.
const ROOT_PADDING: f64 = 30.0;
/// Gap inside a host, around its projects.
const HOST_PADDING: f64 = 22.0;
/// Gap inside a project, around its worktrees (or sessions, when flat).
const PROJECT_PADDING: f64 = 16.0;
/// Gap inside a worktree, around its sessions.
const WORKTREE_PADDING: f64 = 6.0;

/// One session the map places, and where it runs.
pub struct SceneSession<'a> {
    pub id: SessionId,
    pub workspace: Workspace<'a>,
    pub project_slug: Cow<'a, str>,
}

/// A subagent the map draws in orbit around the session running it.
pub struct Satellite {
    pub id: SessionId,
    pub parent: SessionId,
}

/// The tree the map is packed from: host → project → worktree → session,
/// every level sorted by stable key.
#[derive(Debug, Default)]
pub struct SceneTree {
    pub hosts: Vec<HostNode>,
    /// Subagents, by parent then id.
    pub satellites: Vec<Satellite>,
}

#[derive(Debug)]
pub struct HostNode {
    pub name: String,
    pub projects: Vec<ProjectNode>,
}

#[derive(Debug)]
pub struct ProjectNode {
    pub slug: String,
    pub root: PathBuf,
    pub worktrees: Vec<WorktreeNode>,
}

impl ProjectNode {
    /// One worktree, at the project root: drawn as the project's area alone,
    /// like the session list's flat [`ProjectGroup`](crate::session::ProjectGroup).
    pub fn is_flat(&self) -> bool {
        self.worktrees.len() == 1 && self.worktrees[0].cwd == self.root
    }
}

#[derive(Debug)]
pub struct WorktreeNode {
    pub cwd: PathBuf,
    pub sessions: Vec<SessionId>,
}

impl std::fmt::Debug for Satellite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.id, self.parent)
    }
}

impl SceneTree {
    /// Group `sessions` into the tree, sorted by host name, project root,
    /// cwd and session id, so the input's order never shows in the map.
    pub fn new<'a>(
        sessions: impl IntoIterator<Item = SceneSession<'a>>,
        satellites: impl IntoIterator<Item = Satellite>,
    ) -> Self {
        let mut hosts: Vec<HostNode> = Vec::new();
        for s in sessions {
            let ws = s.workspace;
            let host = match hosts.iter_mut().position(|h| h.name == ws.host) {
                Some(i) => &mut hosts[i],
                None => {
                    hosts.push(HostNode {
                        name: ws.host.to_owned(),
                        projects: Vec::new(),
                    });
                    hosts.last_mut().unwrap()
                }
            };
            let project = match host.projects.iter().position(|p| p.root == ws.project_root) {
                Some(i) => &mut host.projects[i],
                None => {
                    host.projects.push(ProjectNode {
                        slug: s.project_slug.into_owned(),
                        root: ws.project_root.to_owned(),
                        worktrees: Vec::new(),
                    });
                    host.projects.last_mut().unwrap()
                }
            };
            match project.worktrees.iter_mut().find(|w| w.cwd == ws.cwd) {
                Some(w) => w.sessions.push(s.id),
                None => project.worktrees.push(WorktreeNode {
                    cwd: ws.cwd.to_owned(),
                    sessions: vec![s.id],
                }),
            }
        }

        hosts.sort_by(|a, b| a.name.cmp(&b.name));
        for host in &mut hosts {
            host.projects.sort_by(|a, b| a.root.cmp(&b.root));
            for project in &mut host.projects {
                project.worktrees.sort_by(|a, b| a.cwd.cmp(&b.cwd));
                for worktree in &mut project.worktrees {
                    worktree.sessions.sort_unstable();
                }
            }
        }

        let mut satellites: Vec<Satellite> = satellites.into_iter().collect();
        satellites.sort_by_key(|s| (s.parent, s.id));
        SceneTree { hosts, satellites }
    }

    /// The tree of every session the scene draws: subagents as satellites
    /// of the session running them, the rest by where they run.
    pub fn from_sessions(sessions: &SessionManager) -> Self {
        let members = || sessions.iter().filter(|s| on_map(s));
        SceneTree::new(
            members()
                .filter(|s| s.subagent_of.is_none())
                .map(|s| SceneSession {
                    id: s.id,
                    workspace: s.workspace(),
                    project_slug: s.project_slug(),
                }),
            members().filter_map(|s| {
                Some(Satellite {
                    id: s.id,
                    parent: s.subagent_of.as_ref()?.parent,
                })
            }),
        )
    }
}

/// The scene draws agentic sessions only.
fn on_map(session: &ChatSession) -> bool {
    session.agentic.is_some()
}

/// A hash of what the map's layout depends on: which sessions it holds,
/// where each runs, and which session each subagent orbits. Nothing else, so
/// a status or title change never relayouts. Order-independent, and runs
/// without allocating, so it can be checked every frame.
pub fn membership(sessions: &SessionManager) -> u64 {
    sessions
        .iter()
        .filter(|s| on_map(s))
        .map(|s| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.id.hash(&mut h);
            s.workspace().hash(&mut h);
            s.subagent_of.as_ref().map(|l| l.parent).hash(&mut h);
            h.finish()
        })
        .fold(0u64, u64::wrapping_add)
}

/// What an area of the map stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaKind {
    Host,
    Project,
    Worktree,
}

/// One territory of the map.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutArea {
    pub kind: AreaKind,
    /// Host name, project slug, or worktree directory name.
    pub label: String,
    /// Stable across relayouts: the area's path through the tree, one line
    /// per level, so an area can ease from where it was.
    pub key: String,
    pub center: Pos2,
    pub radius: f32,
}

/// Where everything on the map goes, in scene coordinates centered on the
/// origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneLayout {
    /// Outermost first, so drawing in order puts children on top.
    pub areas: Vec<LayoutArea>,
    /// Each session's center, subagents included.
    pub nodes: HashMap<SessionId, Pos2>,
}

impl SceneLayout {
    /// Pack `tree` bottom-up: sessions into worktrees, worktrees into
    /// projects, projects into hosts, hosts around the origin.
    #[profiling::function]
    pub fn pack(tree: &SceneTree) -> Self {
        let mut root = PackNode::branch(None, ROOT_PADDING, Vec::new());
        root.children = tree.hosts.iter().map(host_node).collect();
        root.pack();

        let mut layout = SceneLayout::default();
        root.place(0.0, 0.0, &mut layout);

        for (i, sat) in tree.satellites.iter().enumerate() {
            let Some(parent) = layout.nodes.get(&sat.parent).copied() else {
                continue;
            };
            // Siblings of one parent spread by the golden angle, so a new one
            // never lands on an old one.
            let nth = tree.satellites[..i]
                .iter()
                .filter(|s| s.parent == sat.parent)
                .count();
            let angle = -std::f32::consts::FRAC_PI_4 + nth as f32 * 2.399_963;
            let offset = egui::vec2(angle.cos(), angle.sin()) * SATELLITE_ORBIT;
            layout.nodes.insert(sat.id, parent + offset);
        }
        layout
    }

    /// The smallest square around every area, for fitting the camera.
    pub fn bounds(&self) -> Option<egui::Rect> {
        self.areas
            .iter()
            .map(|a| egui::Rect::from_center_size(a.center, egui::Vec2::splat(a.radius * 2.0)))
            .reduce(|a, b| a.union(b))
    }
}

/// How long nodes and areas take to ease to a new layout, in seconds.
pub const EASE_SECS: f64 = 0.3;

/// The map's layout, kept across frames: repacked only when
/// [`membership`] changes, with what is drawn easing from the old layout to
/// the new one over [`EASE_SECS`].
#[derive(Default)]
pub struct EasedLayout {
    membership: Option<u64>,
    layout: SceneLayout,
    /// Where each session was drawn when the current layout landed.
    from_nodes: HashMap<SessionId, Pos2>,
    /// Where each of the layout's areas was drawn then, parallel to
    /// [`SceneLayout::areas`]; `None` for a new area.
    from_areas: Vec<Option<AreaAt>>,
    /// When the current layout landed, in egui seconds.
    landed: f64,
}

/// Where an area is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AreaAt {
    pub center: Pos2,
    pub radius: f32,
}

/// What [`EasedLayout::sync`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synced {
    /// Membership is unchanged; nothing was packed.
    Same,
    /// Repacked; the map eases to the new layout.
    Relaid,
    /// The first layout: nothing to ease from.
    First,
}

impl EasedLayout {
    /// Repack if the sessions the map holds, or where they run, changed
    /// since the last call. Cheap when nothing did.
    pub fn sync(&mut self, sessions: &SessionManager, now: f64) -> Synced {
        let m = membership(sessions);
        if self.membership == Some(m) {
            return Synced::Same;
        }
        let first = self.membership.is_none();
        self.membership = Some(m);
        self.land(SceneLayout::pack(&SceneTree::from_sessions(sessions)), now);
        if first {
            Synced::First
        } else {
            Synced::Relaid
        }
    }

    /// Make `layout` the target, easing from wherever things are drawn at
    /// `now`.
    pub fn land(&mut self, layout: SceneLayout, now: f64) {
        let from_nodes = self
            .layout
            .nodes
            .keys()
            .filter_map(|&id| Some((id, self.node(id, now)?)))
            .collect();
        let from_areas = layout
            .areas
            .iter()
            .map(|new| {
                let i = self.layout.areas.iter().position(|a| a.key == new.key)?;
                Some(self.area(i, now))
            })
            .collect();
        self.from_nodes = from_nodes;
        self.from_areas = from_areas;
        self.layout = layout;
        self.landed = now;
    }

    /// The layout being eased to.
    pub fn target(&self) -> &SceneLayout {
        &self.layout
    }

    /// Whether anything is still moving at `now`, so the caller knows to
    /// repaint.
    pub fn is_easing(&self, now: f64) -> bool {
        now - self.landed < EASE_SECS
            && (!self.from_nodes.is_empty() || self.from_areas.iter().any(Option::is_some))
    }

    /// Where session `id` is drawn at `now`, if it is on the map.
    pub fn node(&self, id: SessionId, now: f64) -> Option<Pos2> {
        let target = *self.layout.nodes.get(&id)?;
        Some(match self.from_nodes.get(&id) {
            Some(from) => from.lerp(target, self.progress(now)),
            None => target,
        })
    }

    /// Where the `i`th of [`target`](Self::target)'s areas is drawn at `now`.
    pub fn area(&self, i: usize, now: f64) -> AreaAt {
        let a = &self.layout.areas[i];
        let target = AreaAt {
            center: a.center,
            radius: a.radius,
        };
        let Some(Some(from)) = self.from_areas.get(i) else {
            return target;
        };
        let t = self.progress(now);
        AreaAt {
            center: from.center.lerp(target.center, t),
            radius: egui::lerp(from.radius..=target.radius, t),
        }
    }

    fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.landed) / EASE_SECS).clamp(0.0, 1.0) as f32;
        1.0 - (1.0 - t).powi(3)
    }
}

/// An area or a session while packing. Children's circles are relative to
/// their parent's center until [`PackNode::place`].
struct PackNode {
    area: Option<AreaInfo>,
    session: Option<SessionId>,
    padding: f64,
    children: Vec<PackNode>,
    circle: Circle,
}

struct AreaInfo {
    kind: AreaKind,
    label: String,
    key: String,
}

impl PackNode {
    fn leaf(id: SessionId) -> Self {
        PackNode {
            area: None,
            session: Some(id),
            padding: 0.0,
            children: Vec::new(),
            circle: Circle {
                x: 0.0,
                y: 0.0,
                r: LEAF_RADIUS as f64,
            },
        }
    }

    fn branch(area: Option<AreaInfo>, padding: f64, children: Vec<PackNode>) -> Self {
        PackNode {
            area,
            session: None,
            padding,
            children,
            circle: Circle::default(),
        }
    }

    /// d3's `packChildren`: pad every child, pack the padded circles, take
    /// the padding back off, and enclose them.
    fn pack(&mut self) {
        if self.session.is_some() {
            return;
        }
        for child in &mut self.children {
            child.pack();
        }
        let pad = self.padding;
        let mut circles: Vec<Circle> = self
            .children
            .iter()
            .map(|c| Circle {
                r: c.circle.r + pad,
                ..c.circle
            })
            .collect();
        let enclosing = pack_siblings(&mut circles);
        for (child, c) in self.children.iter_mut().zip(circles) {
            child.circle = Circle {
                r: child.circle.r,
                ..c
            };
        }
        self.circle.r = enclosing + pad;
    }

    /// Make every center absolute and write the areas and nodes out.
    fn place(self, px: f64, py: f64, layout: &mut SceneLayout) {
        let x = px + self.circle.x;
        let y = py + self.circle.y;
        let center = Pos2::new(x as f32, y as f32);
        if let Some(id) = self.session {
            layout.nodes.insert(id, center);
            return;
        }
        if let Some(area) = self.area {
            layout.areas.push(LayoutArea {
                kind: area.kind,
                label: area.label,
                key: area.key,
                center,
                radius: self.circle.r as f32,
            });
        }
        for child in self.children {
            child.place(x, y, layout);
        }
    }
}

fn host_node(host: &HostNode) -> PackNode {
    let key = format!("h:{}", host.name);
    let projects = host
        .projects
        .iter()
        .map(|p| project_node(p, &key))
        .collect();
    let label = if host.name.is_empty() {
        "local".to_owned()
    } else {
        host.name.clone()
    };
    PackNode::branch(
        Some(AreaInfo {
            kind: AreaKind::Host,
            label,
            key,
        }),
        HOST_PADDING,
        projects,
    )
}

fn project_node(project: &ProjectNode, host_key: &str) -> PackNode {
    let key = format!("{host_key}\np:{}", project.root.display());
    let children = if project.is_flat() {
        leaves(&project.worktrees[0].sessions)
    } else {
        project
            .worktrees
            .iter()
            .map(|w| worktree_node(w, &key))
            .collect()
    };
    PackNode::branch(
        Some(AreaInfo {
            kind: AreaKind::Project,
            label: project.slug.clone(),
            key,
        }),
        PROJECT_PADDING,
        children,
    )
}

fn worktree_node(worktree: &WorktreeNode, project_key: &str) -> PackNode {
    let label = worktree
        .cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| worktree.cwd.display().to_string());
    PackNode::branch(
        Some(AreaInfo {
            kind: AreaKind::Worktree,
            label,
            key: format!("{project_key}\nw:{}", worktree.cwd.display()),
        }),
        WORKTREE_PADDING,
        leaves(&worktree.sessions),
    )
}

fn leaves(ids: &[SessionId]) -> Vec<PackNode> {
    ids.iter().copied().map(PackNode::leaf).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Circle {
    x: f64,
    y: f64,
    r: f64,
}

/// d3's `place`: put `c` tangent to both `a` and `b`.
fn place(b: Circle, a: Circle, c: &mut Circle) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let d2 = dx * dx + dy * dy;
    if d2 == 0.0 {
        c.x = a.x + c.r;
        c.y = a.y;
        return;
    }
    let a2 = (a.r + c.r) * (a.r + c.r);
    let b2 = (b.r + c.r) * (b.r + c.r);
    if a2 > b2 {
        let x = (d2 + b2 - a2) / (2.0 * d2);
        let y = (b2 / d2 - x * x).max(0.0).sqrt();
        c.x = b.x - x * dx - y * dy;
        c.y = b.y - x * dy + y * dx;
    } else {
        let x = (d2 + a2 - b2) / (2.0 * d2);
        let y = (a2 / d2 - x * x).max(0.0).sqrt();
        c.x = a.x + x * dx - y * dy;
        c.y = a.y + x * dy + y * dx;
    }
}

fn intersects(a: Circle, b: Circle) -> bool {
    let dr = a.r + b.r - 1e-6;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dr > 0.0 && dr * dr > dx * dx + dy * dy
}

/// How far the weighted midpoint of a front-chain pair is from the origin.
fn score(a: Circle, b: Circle) -> f64 {
    let ab = a.r + b.r;
    let dx = (a.x * b.r + b.x * a.r) / ab;
    let dy = (a.y * b.r + b.y * a.r) / ab;
    dx * dx + dy * dy
}

/// d3's `packSiblings`: place `circles` tangent to each other around the
/// origin, in order, without overlap, then center their enclosing circle on
/// the origin. Returns the enclosing radius.
fn pack_siblings(circles: &mut [Circle]) -> f64 {
    let n = circles.len();
    if n == 0 {
        return 0.0;
    }
    circles[0].x = 0.0;
    circles[0].y = 0.0;
    if n == 1 {
        return circles[0].r;
    }
    circles[0].x = -circles[1].r;
    circles[1].x = circles[0].r;
    circles[1].y = 0.0;
    if n == 2 {
        return circles[0].r + circles[1].r;
    }
    let (b0, a0) = (circles[1], circles[0]);
    place(b0, a0, &mut circles[2]);

    // The front chain, a ring of indices into `circles`.
    let mut next = vec![0usize; n];
    let mut prev = vec![0usize; n];
    let (mut a, mut b) = (0usize, 1usize);
    next[0] = 1;
    prev[1] = 0;
    next[1] = 2;
    prev[2] = 1;
    next[2] = 0;
    prev[0] = 2;

    let mut i = 3;
    'pack: while i < n {
        let (cb, ca) = (circles[b], circles[a]);
        place(ca, cb, &mut circles[i]);
        let c = circles[i];

        // The closest circle on the front chain that intersects c, by
        // distance along the chain, ahead of b or behind a.
        let (mut j, mut k) = (next[b], prev[a]);
        let (mut sj, mut sk) = (circles[b].r, circles[a].r);
        loop {
            if sj <= sk {
                if intersects(circles[j], c) {
                    b = j;
                    next[a] = b;
                    prev[b] = a;
                    continue 'pack;
                }
                sj += circles[j].r;
                j = next[j];
            } else {
                if intersects(circles[k], c) {
                    a = k;
                    next[a] = b;
                    prev[b] = a;
                    continue 'pack;
                }
                sk += circles[k].r;
                k = prev[k];
            }
            if j == next[k] {
                break;
            }
        }

        // Insert c between a and b.
        prev[i] = a;
        next[i] = b;
        next[a] = i;
        prev[b] = i;
        b = i;

        // The pair closest to the centroid becomes the next a, b.
        let mut best = score(circles[a], circles[next[a]]);
        let mut node = next[b];
        while node != b {
            let s = score(circles[node], circles[next[node]]);
            if s < best {
                a = node;
                best = s;
            }
            node = next[node];
        }
        b = next[a];
        i += 1;
    }

    // Enclose the front chain and center it on the origin.
    let mut chain = vec![circles[b]];
    let mut node = next[b];
    while node != b {
        chain.push(circles[node]);
        node = next[node];
    }
    let e = pack_enclose(&mut chain);
    for c in circles.iter_mut() {
        c.x -= e.x;
        c.y -= e.y;
    }
    e.r
}

/// d3's `packEnclose`: the smallest circle enclosing `circles`, by Welzl's
/// algorithm over a shuffled order. The shuffle is seeded the same every
/// call, so the result is deterministic.
fn pack_enclose(circles: &mut [Circle]) -> Circle {
    shuffle(circles);
    let mut basis: Vec<Circle> = Vec::new();
    let mut e: Option<Circle> = None;
    let mut i = 0;
    while i < circles.len() {
        let p = circles[i];
        if e.is_some_and(|e| encloses_weak(e, p)) {
            i += 1;
            continue;
        }
        basis = match extend_basis(&basis, p) {
            Some(b) => b,
            // Numerically stuck: fall back to a circle around everything.
            None => return bounding(circles),
        };
        e = Some(enclose_basis(&basis));
        i = 0;
    }
    e.unwrap_or_default()
}

/// d3's LCG shuffle (`lcg.js`), seeded at 1.
fn shuffle(circles: &mut [Circle]) {
    let mut s: u64 = 1;
    let mut m = circles.len();
    while m > 0 {
        s = (1_664_525 * s + 1_013_904_223) % 4_294_967_296;
        let r = s as f64 / 4_294_967_296.0;
        let i = (r * m as f64) as usize;
        m -= 1;
        circles.swap(m, i);
    }
}

fn bounding(circles: &[Circle]) -> Circle {
    let n = circles.len().max(1) as f64;
    let x = circles.iter().map(|c| c.x).sum::<f64>() / n;
    let y = circles.iter().map(|c| c.y).sum::<f64>() / n;
    let r = circles
        .iter()
        .map(|c| ((c.x - x).powi(2) + (c.y - y).powi(2)).sqrt() + c.r)
        .fold(0.0, f64::max);
    Circle { x, y, r }
}

fn extend_basis(basis: &[Circle], p: Circle) -> Option<Vec<Circle>> {
    if encloses_weak_all(p, basis) {
        return Some(vec![p]);
    }
    for &bi in basis {
        if encloses_not(p, bi) && encloses_weak_all(enclose_basis2(bi, p), basis) {
            return Some(vec![bi, p]);
        }
    }
    for i in 0..basis.len().saturating_sub(1) {
        for j in i + 1..basis.len() {
            let (bi, bj) = (basis[i], basis[j]);
            if encloses_not(enclose_basis2(bi, bj), p)
                && encloses_not(enclose_basis2(bi, p), bj)
                && encloses_not(enclose_basis2(bj, p), bi)
                && encloses_weak_all(enclose_basis3(bi, bj, p), basis)
            {
                return Some(vec![bi, bj, p]);
            }
        }
    }
    None
}

fn encloses_not(a: Circle, b: Circle) -> bool {
    let dr = a.r - b.r;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dr < 0.0 || dr * dr < dx * dx + dy * dy
}

fn encloses_weak(a: Circle, b: Circle) -> bool {
    let dr = a.r - b.r + a.r.max(b.r).max(1.0) * 1e-9;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dr > 0.0 && dr * dr > dx * dx + dy * dy
}

fn encloses_weak_all(a: Circle, basis: &[Circle]) -> bool {
    basis.iter().all(|&b| encloses_weak(a, b))
}

fn enclose_basis(basis: &[Circle]) -> Circle {
    match basis {
        [a] => *a,
        [a, b] => enclose_basis2(*a, *b),
        [a, b, c] => enclose_basis3(*a, *b, *c),
        _ => Circle::default(),
    }
}

fn enclose_basis2(a: Circle, b: Circle) -> Circle {
    let (x21, y21, r21) = (b.x - a.x, b.y - a.y, b.r - a.r);
    let l = (x21 * x21 + y21 * y21).sqrt();
    Circle {
        x: (a.x + b.x + x21 / l * r21) / 2.0,
        y: (a.y + b.y + y21 / l * r21) / 2.0,
        r: (l + a.r + b.r) / 2.0,
    }
}

fn enclose_basis3(a: Circle, b: Circle, c: Circle) -> Circle {
    let (x1, y1, r1) = (a.x, a.y, a.r);
    let (x2, y2, r2) = (b.x, b.y, b.r);
    let (x3, y3, r3) = (c.x, c.y, c.r);
    let a2 = x1 - x2;
    let a3 = x1 - x3;
    let b2 = y1 - y2;
    let b3 = y1 - y3;
    let c2 = r2 - r1;
    let c3 = r3 - r1;
    let d1 = x1 * x1 + y1 * y1 - r1 * r1;
    let d2 = d1 - x2 * x2 - y2 * y2 + r2 * r2;
    let d3 = d1 - x3 * x3 - y3 * y3 + r3 * r3;
    let ab = a3 * b2 - a2 * b3;
    let xa = (b2 * d3 - b3 * d2) / (ab * 2.0) - x1;
    let xb = (b3 * c2 - b2 * c3) / ab;
    let ya = (a3 * d2 - a2 * d3) / (ab * 2.0) - y1;
    let yb = (a2 * c3 - a3 * c2) / ab;
    let qa = xb * xb + yb * yb - 1.0;
    let qb = 2.0 * (r1 + xa * xb + ya * yb);
    let qc = xa * xa + ya * ya - r1 * r1;
    let r = -(if qa.abs() > 1e-6 {
        (qb + (qb * qb - 4.0 * qa * qc).sqrt()) / (2.0 * qa)
    } else {
        qc / qb
    });
    Circle {
        x: x1 + xa + xb * r,
        y: y1 + ya + yb * r,
        r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct Spec {
        id: SessionId,
        host: &'static str,
        root: &'static str,
        cwd: &'static str,
    }

    fn s(id: SessionId, host: &'static str, root: &'static str, cwd: &'static str) -> Spec {
        Spec {
            id,
            host,
            root,
            cwd,
        }
    }

    fn tree(specs: &[Spec]) -> SceneTree {
        SceneTree::new(
            specs.iter().map(|s| SceneSession {
                id: s.id,
                workspace: Workspace {
                    host: s.host,
                    project_root: Path::new(s.root),
                    cwd: Path::new(s.cwd),
                },
                project_slug: s.root.rsplit('/').next().unwrap().into(),
            }),
            [],
        )
    }

    /// Three hosts, several projects, worktrees and a flat project.
    fn sample() -> Vec<Spec> {
        let mut v = vec![
            s(1, "monad", "/r/notedeck", "/r/notedeck"),
            s(2, "monad", "/r/notedeck", "/r/notedeck-scene"),
            s(3, "monad", "/r/notedeck", "/r/notedeck-scene"),
            s(4, "monad", "/r/damus", "/r/damus"),
            s(5, "hydra", "/u/notedeck", "/u/notedeck-headway"),
            s(6, "hydra", "/u/notedeck", "/u/notedeck-codecol"),
            s(7, "", "/l/x", "/l/x"),
        ];
        for id in 10..22 {
            v.push(s(id, "hydra", "/u/notedeck", "/u/notedeck-horizon"));
        }
        v
    }

    fn dist(a: Pos2, b: Pos2) -> f32 {
        (a - b).length()
    }

    #[test]
    fn siblings_never_overlap() {
        for n in 1..40 {
            let mut circles: Vec<Circle> = (0..n)
                .map(|i| Circle {
                    r: 5.0 + (i * 7 % 13) as f64,
                    ..Default::default()
                })
                .collect();
            let r = pack_siblings(&mut circles);
            for i in 0..n {
                let ci = circles[i];
                let d = (ci.x * ci.x + ci.y * ci.y).sqrt();
                assert!(d + ci.r <= r + 1e-6, "circle {i} of {n} outside enclosure");
                for cj in &circles[i + 1..] {
                    let d = ((ci.x - cj.x).powi(2) + (ci.y - cj.y).powi(2)).sqrt();
                    assert!(d >= ci.r + cj.r - 1e-6, "overlap in {n}");
                }
            }
        }
    }

    #[test]
    fn sessions_do_not_overlap_and_areas_nest() {
        let layout = SceneLayout::pack(&tree(&sample()));
        let nodes: Vec<Pos2> = layout.nodes.values().copied().collect();
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                assert!(dist(*a, *b) >= 2.0 * LEAF_RADIUS - 1e-3);
            }
        }

        // Every area lies inside the area its key nests under, and every
        // session inside the innermost area holding it.
        for area in &layout.areas {
            let Some((parent_key, _)) = area.key.rsplit_once('\n') else {
                continue;
            };
            let parent = layout.areas.iter().find(|a| a.key == parent_key).unwrap();
            assert!(dist(area.center, parent.center) + area.radius <= parent.radius + 1e-3);
        }
        let worktree = layout
            .areas
            .iter()
            .find(|a| a.label == "notedeck-horizon")
            .unwrap();
        for id in 10..22 {
            let p = layout.nodes[&id];
            assert!(dist(p, worktree.center) + LEAF_RADIUS <= worktree.radius + 1e-3);
        }
    }

    #[test]
    fn single_worktree_at_root_flattens() {
        let layout = SceneLayout::pack(&tree(&sample()));
        let kinds = |label: &str| {
            layout
                .areas
                .iter()
                .filter(|a| a.label == label)
                .map(|a| a.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(kinds("damus"), [AreaKind::Project]);
        assert_eq!(kinds("notedeck-scene"), [AreaKind::Worktree]);
        assert_eq!(kinds("local"), [AreaKind::Host]);
    }

    #[test]
    fn same_input_same_output_in_any_order() {
        let a = SceneLayout::pack(&tree(&sample()));
        let mut reversed = sample();
        reversed.reverse();
        let b = SceneLayout::pack(&tree(&reversed));
        assert_eq!(a, b);
    }

    #[test]
    fn a_session_elsewhere_does_not_reshape_another_host() {
        let before = SceneLayout::pack(&tree(&sample()));
        let mut more = sample();
        more.push(s(30, "monad", "/r/notedeck", "/r/notedeck-scene"));
        let after = SceneLayout::pack(&tree(&more));

        // The host as a whole may shift to make room, but nothing inside it
        // moves relative to it.
        let host = |l: &SceneLayout| l.areas.iter().find(|a| a.key == "h:hydra").unwrap().center;
        let (h0, h1) = (host(&before), host(&after));
        for id in [5, 6, 10, 21] {
            let d0 = before.nodes[&id] - h0;
            let d1 = after.nodes[&id] - h1;
            assert!(
                (d0 - d1).length() < 1e-3,
                "session {id} moved within its host"
            );
        }
    }

    #[test]
    fn relayout_eases_from_where_things_were() {
        let mut eased = EasedLayout::default();
        let before = SceneLayout::pack(&tree(&sample()));
        eased.land(before.clone(), 0.0);
        assert!(
            !eased.is_easing(0.0),
            "the first layout has nothing to ease from"
        );

        let mut more = sample();
        more.push(s(30, "hydra", "/u/notedeck", "/u/notedeck-headway"));
        let after = SceneLayout::pack(&tree(&more));
        eased.land(after.clone(), 10.0);

        assert!(eased.is_easing(10.1));
        assert_eq!(eased.node(5, 10.0), Some(before.nodes[&5]));
        assert_eq!(eased.node(5, 10.0 + EASE_SECS), Some(after.nodes[&5]));
        // A new session appears where it belongs.
        assert_eq!(eased.node(30, 10.0), Some(after.nodes[&30]));
        assert!(!eased.is_easing(10.0 + EASE_SECS));
    }

    #[test]
    fn satellites_orbit_their_parent() {
        let t = SceneTree::new(
            [SceneSession {
                id: 1,
                workspace: Workspace {
                    host: "",
                    project_root: Path::new("/a"),
                    cwd: Path::new("/a"),
                },
                project_slug: "a".into(),
            }],
            [
                Satellite { id: 2, parent: 1 },
                Satellite { id: 3, parent: 1 },
            ],
        );
        let layout = SceneLayout::pack(&t);
        for id in [2, 3] {
            assert!((dist(layout.nodes[&id], layout.nodes[&1]) - SATELLITE_ORBIT).abs() < 1e-3);
        }
        assert!(dist(layout.nodes[&2], layout.nodes[&3]) > 1.0);
    }
}
