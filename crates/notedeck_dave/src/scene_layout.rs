//! Where the scene map draws each session: hosts, projects and worktrees as
//! nested territories, sessions as points inside them. Territories are
//! rectangles cut by a squarified treemap ([`SceneLayout::treemap`], the
//! default) or circles placed by circle packing ([`SceneLayout::pack`]); see
//! [`SceneLayoutMode`].
//!
//! Pure, using egui only for its geometry types. [`SceneTree::new`] sorts
//! what it is given by stable keys, so the same sessions always give the
//! same map.
//!
//! The treemap is Bruls, Huizing and van Wijk's squarified treemap (2000),
//! cut in key order rather than by size so siblings keep their places.
//!
//! The packing is a port of d3-hierarchy's `packSiblings` (front-chain
//! placement, Wang et al. 2006) and `packEnclose` (Welzl's smallest enclosing
//! circle), <https://github.com/d3/d3-hierarchy>, Copyright 2010-2021 Mike
//! Bostock, ISC license.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use egui::{Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::session::{ChatSession, SessionId, SessionManager, Workspace};

/// Radius of a session's circle: room for a 15 px status ring and the
/// satellites that orbit it (see [`crate::scene_orbits`]).
pub const LEAF_RADIUS: f32 = 28.0;

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

/// The tree the map is packed from: host → project → worktree → session,
/// every level sorted by stable key.
#[derive(Debug, Default)]
pub struct SceneTree {
    pub hosts: Vec<HostNode>,
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

impl SceneTree {
    /// Group `sessions` into the tree, sorted by host name, project root,
    /// cwd and session id, so the input's order never shows in the map.
    pub fn new<'a>(sessions: impl IntoIterator<Item = SceneSession<'a>>) -> Self {
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

        SceneTree { hosts }
    }

    /// The tree of every session the scene places, by where they run.
    /// Subagents aren't placed: they orbit the session running them (see
    /// [`crate::scene_orbits`]).
    pub fn from_sessions(sessions: &SessionManager) -> Self {
        SceneTree::new(sessions.iter().filter(|s| on_map(s)).map(|s| SceneSession {
            id: s.id,
            workspace: s.workspace(),
            project_slug: s.project_slug(),
        }))
    }
}

/// The scene places agentic sessions only, and no subagent.
fn on_map(session: &ChatSession) -> bool {
    session.agentic.is_some() && session.subagent_of.is_none()
}

/// A hash of what the map's layout depends on: which sessions it holds,
/// and where each runs. Nothing else, so
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

impl AreaKind {
    /// Height of the strip along a treemap rectangle's top that holds its
    /// label.
    pub fn header(self) -> f32 {
        match self {
            AreaKind::Host => 22.0,
            AreaKind::Project => 18.0,
            AreaKind::Worktree => 15.0,
        }
    }

    /// Gap between a treemap rectangle's edge and what it holds, on the
    /// sides and bottom; the label is inset by it too.
    pub fn inset(self) -> f32 {
        match self {
            AreaKind::Host => 10.0,
            AreaKind::Project => 8.0,
            AreaKind::Worktree => 4.0,
        }
    }
}

/// How the map lays out its territories. Persisted, so it is the user's
/// choice across restarts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneLayoutMode {
    /// Rectangles that fill the panel: a squarified treemap.
    #[default]
    Treemap,
    /// Nested circles, placed by circle packing.
    Circles,
}

/// The shape of one territory, as laid out or as drawn mid-ease.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AreaShape {
    Rect(Rect),
    Circle { center: Pos2, radius: f32 },
}

impl AreaShape {
    /// The smallest rect around the shape.
    pub fn bounding_rect(&self) -> Rect {
        match *self {
            AreaShape::Rect(rect) => rect,
            AreaShape::Circle { center, radius } => {
                Rect::from_center_size(center, Vec2::splat(radius * 2.0))
            }
        }
    }

    /// `t` of the way from `self` to `to`, in `to`'s shape: a circle easing
    /// to a rect starts from its bounding square, a rect easing to a circle
    /// from the circle that fits inside it.
    pub fn lerp(&self, to: AreaShape, t: f32) -> AreaShape {
        match (*self, to) {
            (
                AreaShape::Circle {
                    center: c0,
                    radius: r0,
                },
                AreaShape::Circle {
                    center: c1,
                    radius: r1,
                },
            ) => AreaShape::Circle {
                center: c0.lerp(c1, t),
                radius: egui::lerp(r0..=r1, t),
            },
            (from, AreaShape::Rect(to)) => {
                AreaShape::Rect(from.bounding_rect().lerp_towards(&to, t))
            }
            (AreaShape::Rect(from), AreaShape::Circle { center, radius }) => AreaShape::Circle {
                center: from.center().lerp(center, t),
                radius: egui::lerp(from.size().min_elem() / 2.0..=radius, t),
            },
        }
    }
}

/// One territory of the map.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutArea {
    pub kind: AreaKind,
    /// Host name in capitals, project slug, or worktree directory name.
    pub label: String,
    /// Stable across relayouts: the area's path through the tree, one line
    /// per level, so an area can ease from where it was.
    pub key: String,
    pub shape: AreaShape,
}

/// Where everything on the map goes, in scene coordinates centered on the
/// origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneLayout {
    /// Outermost first, so drawing in order puts children on top.
    pub areas: Vec<LayoutArea>,
    /// Each placed session's center.
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

        layout
    }

    /// Cut `tree` into a squarified treemap the shape of a `panel` of that
    /// size, centered on the origin. Each group weighs its sessions plus
    /// [`HEADER_WEIGHT`] for its header; sessions sit in a grid in their
    /// leaf, every one in a slot of the same size.
    ///
    /// The map is cut at the panel's size scaled up just enough that the
    /// tightest leaf's slot is at least a session's ([`LEAF_RADIUS`] × 2),
    /// so nodes, labels and orbits keep the sizes they have in circle
    /// packing and the camera fitting the panel does the shrinking.
    #[profiling::function]
    pub fn treemap(tree: &SceneTree, panel: Vec2) -> Self {
        let groups: Vec<Group<'_>> = tree.hosts.iter().map(host_group).collect();
        let panel = panel.max(MIN_PANEL);
        let cut = |scale: f32| Treemap::cut(&groups, panel * scale);

        let mut best = cut(1.0);
        if best.slot < SLOT {
            // Double until the slot fits, then bisect down to the smallest
            // scale that still fits.
            let (mut lo, mut hi) = (1.0_f32, 2.0_f32);
            loop {
                best = cut(hi);
                if best.slot >= SLOT || hi >= MAX_SCALE {
                    break;
                }
                lo = hi;
                hi *= 2.0;
            }
            for _ in 0..BISECT_STEPS {
                let mid = (lo + hi) / 2.0;
                let tried = cut(mid);
                if tried.slot >= SLOT {
                    hi = mid;
                    best = tried;
                } else {
                    lo = mid;
                }
            }
        }
        best.into_layout()
    }

    /// The smallest rect around every area, for fitting the camera.
    pub fn bounds(&self) -> Option<Rect> {
        self.areas
            .iter()
            .map(|a| a.shape.bounding_rect())
            .reduce(|a, b| a.union(b))
    }
}

/// A session's slot in the treemap: the room its node and orbit need.
const SLOT: f32 = LEAF_RADIUS * 2.0;
/// The largest slot the treemap gives a session when there is room to
/// spare, so a few sessions don't draw far apart in a big panel.
const MAX_SLOT: f32 = SLOT * 1.6;
/// A group header's weight in the treemap, in sessions' worth of area.
const HEADER_WEIGHT: f32 = 0.6;
/// Gap between the panel's edge and the hosts.
const TREEMAP_MARGIN: f32 = 14.0;
/// How far each rectangle is pulled in from its share, so siblings don't
/// share an edge.
const TREEMAP_GAP: f32 = 2.0;
/// The panel size the treemap lays out for at the least, so a panel not
/// sized yet still gives a sane map.
const MIN_PANEL: Vec2 = Vec2::splat(200.0);
/// The most the treemap scales the panel up looking for room.
const MAX_SCALE: f32 = 1024.0;
/// Bisection steps for the treemap's scale: well under a slot's precision.
const BISECT_STEPS: usize = 12;

/// A group of the tree as the treemap cuts it: an area, and either the
/// groups inside it or, for a leaf, its sessions.
struct Group<'a> {
    area: AreaInfo,
    children: Vec<Group<'a>>,
    sessions: &'a [SessionId],
    /// Its sessions plus a [`HEADER_WEIGHT`] for every header in it.
    weight: f32,
}

impl<'a> Group<'a> {
    fn branch(area: AreaInfo, children: Vec<Group<'a>>) -> Self {
        let weight = HEADER_WEIGHT + children.iter().map(|c| c.weight).sum::<f32>();
        Group {
            area,
            children,
            sessions: &[],
            weight,
        }
    }

    fn leaf(area: AreaInfo, sessions: &'a [SessionId]) -> Self {
        Group {
            area,
            children: Vec::new(),
            weight: HEADER_WEIGHT + sessions.len() as f32,
            sessions,
        }
    }
}

fn host_group(host: &HostNode) -> Group<'_> {
    let area = host_info(host);
    let projects = host
        .projects
        .iter()
        .map(|p| project_group(p, &area.key))
        .collect();
    Group::branch(area, projects)
}

fn project_group<'a>(project: &'a ProjectNode, host_key: &str) -> Group<'a> {
    let area = project_info(project, host_key);
    if project.is_flat() {
        return Group::leaf(area, &project.worktrees[0].sessions);
    }
    let worktrees = project
        .worktrees
        .iter()
        .map(|w| Group::leaf(worktree_info(w, &area.key), &w.sessions))
        .collect();
    Group::branch(area, worktrees)
}

/// One cut of the treemap at one scale.
struct Treemap<'a> {
    areas: Vec<LayoutArea>,
    leaves: Vec<Leaf<'a>>,
    /// The slot every session gets: the tightest leaf's, at most
    /// [`MAX_SLOT`].
    slot: f32,
}

/// Where a leaf's sessions go.
struct Leaf<'a> {
    sessions: &'a [SessionId],
    /// Inside the leaf's header and inset.
    rect: Rect,
}

impl<'a> Treemap<'a> {
    fn cut(groups: &[Group<'a>], size: Vec2) -> Self {
        let mut map = Treemap {
            areas: Vec::new(),
            leaves: Vec::new(),
            slot: MAX_SLOT,
        };
        let root = Rect::from_center_size(Pos2::ZERO, size).shrink(TREEMAP_MARGIN);
        map.cut_children(groups, root);
        for leaf in &map.leaves {
            map.slot = map
                .slot
                .min(grid_cell(leaf.sessions.len(), leaf.rect.size()));
        }
        map
    }

    fn cut_children(&mut self, groups: &[Group<'a>], rect: Rect) {
        let weights: Vec<f32> = groups.iter().map(|g| g.weight).collect();
        for (group, share) in groups.iter().zip(squarify(&weights, rect)) {
            self.cut_group(group, share.shrink(TREEMAP_GAP));
        }
    }

    fn cut_group(&mut self, group: &Group<'a>, rect: Rect) {
        let kind = group.area.kind;
        self.areas.push(LayoutArea {
            kind,
            label: group.area.label.clone(),
            key: group.area.key.clone(),
            shape: AreaShape::Rect(rect),
        });
        let inset = kind.inset();
        let inner = Rect::from_min_max(
            Pos2::new(rect.min.x + inset, rect.min.y + kind.header()),
            Pos2::new(rect.max.x - inset, rect.max.y - inset),
        );
        // Too small to hold anything: a degenerate rect at its corner, so
        // the cut goes on and the slot it gives says this scale is too small.
        let inner = Rect::from_min_size(inner.min, inner.size().max(Vec2::ZERO));
        if group.children.is_empty() {
            self.leaves.push(Leaf {
                sessions: group.sessions,
                rect: inner,
            });
        } else {
            self.cut_children(&group.children, inner);
        }
    }

    /// Put every leaf's sessions in a grid of [`Self::slot`]-sized cells,
    /// centered in the leaf.
    fn into_layout(self) -> SceneLayout {
        let slot = self.slot;
        let mut nodes = HashMap::new();
        for leaf in &self.leaves {
            let n = leaf.sessions.len();
            if n == 0 {
                continue;
            }
            let cols = ((leaf.rect.width() / slot + 1e-4).floor() as usize).clamp(1, n);
            let rows = n.div_ceil(cols);
            let grid = Vec2::new(cols as f32, rows as f32) * slot;
            let origin = leaf.rect.min + (leaf.rect.size() - grid) / 2.0;
            for (i, &id) in leaf.sessions.iter().enumerate() {
                let cell = Vec2::new((i % cols) as f32 + 0.5, (i / cols) as f32 + 0.5);
                nodes.insert(id, origin + cell * slot);
            }
        }
        SceneLayout {
            areas: self.areas,
            nodes,
        }
    }
}

/// The largest square cell `n` sessions can share in a grid inside `size`.
fn grid_cell(n: usize, size: Vec2) -> f32 {
    (1..=n.max(1))
        .map(|cols| {
            let rows = n.max(1).div_ceil(cols);
            (size.x / cols as f32).min(size.y / rows as f32)
        })
        .fold(0.0, f32::max)
}

/// Cut `rect` into one rect per weight, each with its weight's share of the
/// area, as near square as the order allows: Bruls et al.'s squarify,
/// without the sort, so the rects come out in `weights`' order.
///
/// Each row runs along the free rect's shorter side and takes items while
/// that keeps its worst aspect ratio from getting worse. A row's last rect
/// ends on the free rect's edge, and the last row takes all that is left,
/// so rounding never pushes a rect outside `rect`.
fn squarify(weights: &[f32], rect: Rect) -> Vec<Rect> {
    let total: f32 = weights.iter().sum();
    if total <= 0.0 || rect.area() <= 0.0 {
        return vec![Rect::from_min_size(rect.min, Vec2::ZERO); weights.len()];
    }
    let scale = rect.area() / total;
    let areas: Vec<f32> = weights.iter().map(|w| w * scale).collect();

    let mut out = Vec::with_capacity(areas.len());
    let mut free = rect;
    let mut start = 0;
    while start < areas.len() {
        let side = free.width().min(free.height());
        let mut end = start + 1;
        while end < areas.len()
            && worst_ratio(&areas[start..=end], side) <= worst_ratio(&areas[start..end], side)
        {
            end += 1;
        }
        let row = &areas[start..end];
        let last_row = end == areas.len();
        let sum: f32 = row.iter().sum();
        if free.width() >= free.height() {
            // A column down the free rect's left.
            let width = if last_row {
                free.width()
            } else {
                sum / free.height()
            };
            let mut y = free.min.y;
            for (i, a) in row.iter().enumerate() {
                let bottom = if i + 1 == row.len() {
                    free.max.y
                } else {
                    y + a / width
                };
                out.push(Rect::from_min_max(
                    Pos2::new(free.min.x, y),
                    Pos2::new(free.min.x + width, bottom),
                ));
                y = bottom;
            }
            free.min.x += width;
        } else {
            // A row across the free rect's top.
            let height = if last_row {
                free.height()
            } else {
                sum / free.width()
            };
            let mut x = free.min.x;
            for (i, a) in row.iter().enumerate() {
                let right = if i + 1 == row.len() {
                    free.max.x
                } else {
                    x + a / height
                };
                out.push(Rect::from_min_max(
                    Pos2::new(x, free.min.y),
                    Pos2::new(right, free.min.y + height),
                ));
                x = right;
            }
            free.min.y += height;
        }
        start = end;
    }
    out
}

/// The worst aspect ratio of a row of areas laid along a side of `side`.
fn worst_ratio(row: &[f32], side: f32) -> f32 {
    let sum: f32 = row.iter().sum();
    let max = row.iter().copied().fold(0.0, f32::max);
    let min = row.iter().copied().fold(f32::INFINITY, f32::min);
    let side2 = side * side;
    let sum2 = sum * sum;
    (side2 * max / sum2).max(sum2 / (side2 * min))
}

/// How long nodes and areas take to ease to a new layout, in seconds.
pub const EASE_SECS: f64 = 0.3;

/// How much the panel may grow or shrink, as a share of either side, before
/// the treemap is cut again for it, so dragging the split doesn't reflow
/// the map every frame.
const PANEL_RELAYOUT_SHARE: f32 = 0.1;

/// The map's layout, kept across frames: laid out again only when
/// [`membership`] or the mode changes, or for the treemap when the panel
/// changes size by more than [`PANEL_RELAYOUT_SHARE`], with what is drawn
/// easing from the old layout to the new one over [`EASE_SECS`].
#[derive(Default)]
pub struct EasedLayout {
    /// What the current layout was laid out for.
    laid_for: Option<LaidFor>,
    layout: SceneLayout,
    /// Where each session was drawn when the current layout landed.
    from_nodes: HashMap<SessionId, Pos2>,
    /// Where each of the layout's areas was drawn then, parallel to
    /// [`SceneLayout::areas`]; `None` for a new area.
    from_areas: Vec<Option<AreaShape>>,
    /// When the current layout landed, in egui seconds.
    landed: f64,
}

/// What a layout was laid out for: everything that, changed, lays it out
/// again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LaidFor {
    membership: u64,
    mode: SceneLayoutMode,
    /// The panel's size, which only the treemap depends on.
    panel: Vec2,
}

impl LaidFor {
    /// Whether a layout laid out for `self` is still right for `now`.
    fn still_fits(&self, now: &LaidFor) -> bool {
        if self.membership != now.membership || self.mode != now.mode {
            return false;
        }
        if self.mode == SceneLayoutMode::Circles {
            return true;
        }
        let change = (now.panel - self.panel).abs() / self.panel.max(Vec2::splat(1.0));
        change.max_elem() <= PANEL_RELAYOUT_SHARE
    }
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
    /// Lay the map out again, in `mode`, if the sessions it holds or where
    /// they run changed since the last call, or the mode did, or (for the
    /// treemap) the `panel` size moved past [`PANEL_RELAYOUT_SHARE`]. Cheap
    /// when nothing did.
    pub fn sync(
        &mut self,
        sessions: &SessionManager,
        mode: SceneLayoutMode,
        panel: Vec2,
        now: f64,
    ) -> Synced {
        let wanted = LaidFor {
            membership: membership(sessions),
            mode,
            panel,
        };
        if self.laid_for.is_some_and(|laid| laid.still_fits(&wanted)) {
            return Synced::Same;
        }
        let first = self.laid_for.is_none();
        self.laid_for = Some(wanted);
        let tree = SceneTree::from_sessions(sessions);
        let layout = match mode {
            SceneLayoutMode::Treemap => SceneLayout::treemap(&tree, panel),
            SceneLayoutMode::Circles => SceneLayout::pack(&tree),
        };
        self.land(layout, now);
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
    pub fn area(&self, i: usize, now: f64) -> AreaShape {
        let target = self.layout.areas[i].shape;
        let Some(Some(from)) = self.from_areas.get(i) else {
            return target;
        };
        from.lerp(target, self.progress(now))
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
                shape: AreaShape::Circle {
                    center,
                    radius: self.circle.r as f32,
                },
            });
        }
        for child in self.children {
            child.place(x, y, layout);
        }
    }
}

fn host_node(host: &HostNode) -> PackNode {
    let area = host_info(host);
    let projects = host
        .projects
        .iter()
        .map(|p| project_node(p, &area.key))
        .collect();
    PackNode::branch(Some(area), HOST_PADDING, projects)
}

fn project_node(project: &ProjectNode, host_key: &str) -> PackNode {
    let area = project_info(project, host_key);
    let children = if project.is_flat() {
        leaves(&project.worktrees[0].sessions)
    } else {
        project
            .worktrees
            .iter()
            .map(|w| worktree_node(w, &area.key))
            .collect()
    };
    PackNode::branch(Some(area), PROJECT_PADDING, children)
}

fn worktree_node(worktree: &WorktreeNode, project_key: &str) -> PackNode {
    PackNode::branch(
        Some(worktree_info(worktree, project_key)),
        WORKTREE_PADDING,
        leaves(&worktree.sessions),
    )
}

/// A host's area: labelled in capitals, as the map draws it.
fn host_info(host: &HostNode) -> AreaInfo {
    let label = if host.name.is_empty() {
        "LOCAL".to_owned()
    } else {
        host.name.to_uppercase()
    };
    AreaInfo {
        kind: AreaKind::Host,
        label,
        key: format!("h:{}", host.name),
    }
}

fn project_info(project: &ProjectNode, host_key: &str) -> AreaInfo {
    AreaInfo {
        kind: AreaKind::Project,
        label: project.slug.clone(),
        key: format!("{host_key}\np:{}", project.root.display()),
    }
}

fn worktree_info(worktree: &WorktreeNode, project_key: &str) -> AreaInfo {
    let label = worktree
        .cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| worktree.cwd.display().to_string());
    AreaInfo {
        kind: AreaKind::Worktree,
        label,
        key: format!("{project_key}\nw:{}", worktree.cwd.display()),
    }
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
        SceneTree::new(specs.iter().map(|s| SceneSession {
            id: s.id,
            workspace: Workspace {
                host: s.host,
                project_root: Path::new(s.root),
                cwd: Path::new(s.cwd),
            },
            project_slug: s.root.rsplit('/').next().unwrap().into(),
        }))
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

    /// A panel the shape of the scene's half of a laptop window.
    const PANEL: Vec2 = Vec2::new(792.0, 840.0);

    /// `tree` laid out in both modes, the treemap for [`PANEL`].
    fn both(tree: &SceneTree) -> [SceneLayout; 2] {
        [SceneLayout::treemap(tree, PANEL), SceneLayout::pack(tree)]
    }

    /// Whether `inner` lies inside `outer`, give or take rounding.
    fn nests(inner: AreaShape, outer: AreaShape) -> bool {
        match (inner, outer) {
            (AreaShape::Rect(i), AreaShape::Rect(o)) => o.expand(1e-3).contains_rect(i),
            (
                AreaShape::Circle { center, radius },
                AreaShape::Circle {
                    center: oc,
                    radius: or,
                },
            ) => dist(center, oc) + radius <= or + 1e-3,
            _ => false,
        }
    }

    /// A session's slot around `p`, in `area`'s shape.
    fn slot_at(p: Pos2, area: AreaShape) -> AreaShape {
        match area {
            AreaShape::Rect(_) => {
                AreaShape::Rect(Rect::from_center_size(p, Vec2::splat(2.0 * LEAF_RADIUS)))
            }
            AreaShape::Circle { .. } => AreaShape::Circle {
                center: p,
                radius: LEAF_RADIUS,
            },
        }
    }

    #[test]
    fn squarify_tiles_its_rect_in_proportion() {
        let rect = Rect::from_min_size(Pos2::new(3.0, -7.0), Vec2::new(410.0, 230.0));
        for n in 1..40 {
            let weights: Vec<f32> = (0..n).map(|i| 0.6 + (i * 7 % 13) as f32).collect();
            let total: f32 = weights.iter().sum();
            let rects = squarify(&weights, rect);
            assert_eq!(rects.len(), n);
            for (i, (r, w)) in rects.iter().zip(&weights).enumerate() {
                assert!(rect.contains_rect(*r), "rect {i} of {n} outside");
                let share = r.area() / rect.area();
                assert!((share - w / total).abs() < 1e-3, "rect {i} of {n}: {share}");
                for other in &rects[i + 1..] {
                    assert!(r.intersect(*other).area() < 1e-2, "overlap in {n}");
                }
            }
        }
    }

    #[test]
    fn sessions_do_not_overlap_and_areas_nest() {
        for layout in both(&tree(&sample())) {
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
                assert!(
                    nests(area.shape, parent.shape),
                    "{} in {}",
                    area.key,
                    parent_key
                );
            }
            let worktree = layout
                .areas
                .iter()
                .find(|a| a.label == "notedeck-horizon")
                .unwrap();
            for id in 10..22 {
                let slot = slot_at(layout.nodes[&id], worktree.shape);
                assert!(nests(slot, worktree.shape), "session {id}");
            }
        }
    }

    #[test]
    fn treemap_fills_the_panel() {
        let layout = SceneLayout::treemap(&tree(&sample()), PANEL);
        assert!(layout
            .areas
            .iter()
            .all(|a| matches!(a.shape, AreaShape::Rect(_))));
        let bounds = layout.bounds().unwrap();
        let aspect = bounds.width() / bounds.height();
        assert!(
            (aspect - PANEL.x / PANEL.y).abs() < 0.05,
            "{aspect} for a panel of {PANEL:?}"
        );
    }

    /// Hundreds of sessions in a small panel: the treemap is cut at a
    /// bigger scale rather than squeezing sessions together.
    #[test]
    fn a_crowded_treemap_scales_up_rather_than_overlap() {
        let mut specs = sample();
        for id in 100..400 {
            let cwd = ["/u/a", "/u/b", "/u/c"][id as usize % 3];
            specs.push(s(id, "hydra", "/u/notedeck", cwd));
        }
        let layout = SceneLayout::treemap(&tree(&specs), Vec2::new(300.0, 200.0));
        assert_eq!(layout.nodes.len(), specs.len());
        let mut nodes: Vec<Pos2> = layout.nodes.values().copied().collect();
        nodes.sort_by(|a, b| a.x.total_cmp(&b.x));
        for (i, a) in nodes.iter().enumerate() {
            for b in nodes[i + 1..]
                .iter()
                .take_while(|b| b.x - a.x < 2.0 * LEAF_RADIUS)
            {
                assert!(dist(*a, *b) >= 2.0 * LEAF_RADIUS - 1e-2);
            }
        }
    }

    #[test]
    fn single_worktree_at_root_flattens() {
        for layout in both(&tree(&sample())) {
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
            assert_eq!(kinds("LOCAL"), [AreaKind::Host]);
        }
    }

    #[test]
    fn same_input_same_output_in_any_order() {
        let mut reversed = sample();
        reversed.reverse();
        assert_eq!(both(&tree(&sample())), both(&tree(&reversed)));
    }

    /// The treemap is laid out again for a panel that changed size past the
    /// threshold, the circles never are.
    #[test]
    fn only_a_real_resize_relays_the_treemap() {
        let laid = LaidFor {
            membership: 1,
            mode: SceneLayoutMode::Treemap,
            panel: Vec2::new(800.0, 600.0),
        };
        let at = |panel: Vec2, mode: SceneLayoutMode| LaidFor {
            panel,
            mode,
            ..laid
        };
        assert!(laid.still_fits(&at(Vec2::new(840.0, 580.0), SceneLayoutMode::Treemap)));
        assert!(!laid.still_fits(&at(Vec2::new(900.0, 600.0), SceneLayoutMode::Treemap)));
        assert!(!laid.still_fits(&at(laid.panel, SceneLayoutMode::Circles)));
        assert!(!laid.still_fits(&LaidFor {
            membership: 2,
            ..laid
        }));

        let circles = at(laid.panel, SceneLayoutMode::Circles);
        assert!(circles.still_fits(&at(Vec2::new(300.0, 900.0), SceneLayoutMode::Circles)));
    }

    /// Switching modes eases each circle into its rect.
    #[test]
    fn switching_modes_eases_between_shapes() {
        let t = tree(&sample());
        let mut eased = EasedLayout::default();
        let circles = SceneLayout::pack(&t);
        eased.land(circles.clone(), 0.0);
        let rects = SceneLayout::treemap(&t, PANEL);
        eased.land(rects.clone(), 1.0);

        let i = rects.areas.iter().position(|a| a.key == "h:hydra").unwrap();
        let j = circles
            .areas
            .iter()
            .position(|a| a.key == "h:hydra")
            .unwrap();
        assert_eq!(
            eased.area(i, 1.0),
            AreaShape::Rect(circles.areas[j].shape.bounding_rect())
        );
        assert!(matches!(eased.area(i, 1.1), AreaShape::Rect(_)));
        assert_eq!(eased.area(i, 1.0 + EASE_SECS), rects.areas[i].shape);
    }

    /// Circle packing only: the treemap re-cuts every rect after a weight
    /// that changed, by design.
    #[test]
    fn a_session_elsewhere_does_not_reshape_another_host() {
        let before = SceneLayout::pack(&tree(&sample()));
        let mut more = sample();
        more.push(s(30, "monad", "/r/notedeck", "/r/notedeck-scene"));
        let after = SceneLayout::pack(&tree(&more));

        // The host as a whole may shift to make room, but nothing inside it
        // moves relative to it.
        let host = |l: &SceneLayout| {
            let area = l.areas.iter().find(|a| a.key == "h:hydra").unwrap();
            area.shape.bounding_rect().center()
        };
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
        let mut more = sample();
        more.push(s(30, "hydra", "/u/notedeck", "/u/notedeck-headway"));
        for (before, after) in both(&tree(&sample())).into_iter().zip(both(&tree(&more))) {
            let mut eased = EasedLayout::default();
            eased.land(before.clone(), 0.0);
            assert!(
                !eased.is_easing(0.0),
                "the first layout has nothing to ease from"
            );

            eased.land(after.clone(), 10.0);

            assert!(eased.is_easing(10.1));
            assert_eq!(eased.node(5, 10.0), Some(before.nodes[&5]));
            assert_eq!(eased.node(5, 10.0 + EASE_SECS), Some(after.nodes[&5]));
            // A new session appears where it belongs.
            assert_eq!(eased.node(30, 10.0), Some(after.nodes[&30]));
            assert!(!eased.is_easing(10.0 + EASE_SECS));
        }
    }
}
