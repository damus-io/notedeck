//! Session lineage for the session list: which sessions nest under which, and
//! which run as one successor chain.
//!
//! Two relations put a session somewhere other than the top of its
//! workspace:
//!
//! - a child session (`relation=child`) nests under the session that spawned
//!   it, as a full row;
//! - a chain link follows the session that spawned it in one successor chain
//!   (see [`is_chain_link`]).
//!
//! A Claude subagent's virtual session ([`SubagentOf`](super::SubagentOf))
//! isn't walked: the list draws no row for it, and the scene map orbits it
//! round its parent instead.
//!
//! [`entries`] walks those relations once per regroup into [`Entry`]s, each a
//! top-level session or a chain with everything nested under it, already in
//! draw order. The walk sees each session as a [`Node`], so it is pure and
//! tested without a [`SessionManager`](super::SessionManager).

use super::SessionId;
use agentium_core::session_events::SessionRelation;
use std::collections::HashMap;

/// One session as the lineage walk sees it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Node<'a> {
    pub id: SessionId,
    /// The session that spawned this one, when it is known here: the one its
    /// `parent-session` names.
    pub spawner: Option<SessionId>,
    pub relation: SessionRelation,
    /// Where the session runs. A chain in the list never pulls a session
    /// out of its own workspace (see [`list_chains_in_place`]).
    pub workspace: Workspace<'a>,
    /// The card the session works (its title ref, else its issue url).
    pub card: Option<&'a str>,
    /// The card `card` is a subissue of, as last looked up.
    pub card_parent: Option<&'a str>,
}

/// The host and cwd a session runs in: the workspace folder the list
/// files it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Workspace<'a> {
    pub host: &'a str,
    pub cwd: Option<&'a std::path::Path>,
}

/// How a session hangs under the one before it in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nesting {
    /// At the top of its workspace.
    Top,
    /// A link of a successor chain. `handed_on` when a later link follows
    /// it, so the list dims it as done.
    Link { handed_on: bool },
    /// A child session, under the session that spawned it.
    Child,
}

/// Where one session of an [`Entry`] is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub nesting: Nesting,
    /// How many levels under its entry's top row (or its chain link) it sits.
    pub depth: u8,
}

/// A chain's part of an [`Entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChainShape {
    /// How many links the chain has.
    pub links: usize,
    /// Where in [`Entry::ids`] the last link starts. It and what nests under
    /// it stay visible while the chain is collapsed.
    pub tail: usize,
    /// The card every link's card is a subissue of, when one is known.
    pub epic: Option<String>,
}

/// A top-level session, or a successor chain, with every session nested
/// under it, in draw order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The session whose workspace the entry is listed in: the session
    /// itself, or a chain's last link, where its live work is.
    pub anchor: SessionId,
    pub ids: Vec<SessionId>,
    /// Parallel to `ids`.
    pub places: Vec<Place>,
    pub chain: Option<ChainShape>,
}

/// Whether `node`, spawned by `spawner`, is the next link of the
/// spawner's successor chain.
///
/// Explicitly, when it was spawned as a successor. Inferred, when it says
/// nothing about how it relates (`relation=none`, as every spawn did before
/// `--successor`) and it and its spawner work distinct cards that are
/// subissues of one parent card: work passed from one card of an epic to the
/// next. A child is never a link.
pub(crate) fn is_chain_link(node: &Node<'_>, spawner: &Node<'_>) -> bool {
    match node.relation {
        SessionRelation::Successor => true,
        SessionRelation::Child => false,
        SessionRelation::None => match (node.card, spawner.card) {
            (Some(card), Some(prev)) if card != prev => {
                node.card_parent.is_some() && node.card_parent == spawner.card_parent
            }
            _ => false,
        },
    }
}

/// A session hanging off another, and how.
#[derive(Debug, Clone, Copy)]
struct Kid {
    id: SessionId,
    nesting: Nesting,
}

/// How a node hangs off the session that spawned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attach {
    Top,
    Under { parent: SessionId, nesting: Nesting },
}

fn attach(node: &Node<'_>, nodes: &HashMap<SessionId, Node<'_>>) -> Attach {
    let Some(spawner) = node.spawner.and_then(|id| nodes.get(&id)) else {
        return Attach::Top;
    };
    let nesting = if node.relation == SessionRelation::Child {
        Nesting::Child
    } else if is_chain_link(node, spawner) {
        Nesting::Link { handed_on: false }
    } else {
        return Attach::Top;
    };
    Attach::Under {
        parent: spawner.id,
        nesting,
    }
}

/// A line the scene map draws between two sessions for how one spawned the
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// The spawner.
    pub from: SessionId,
    /// The session it spawned.
    pub to: SessionId,
    pub kind: EdgeKind,
}

/// How an [`Edge`]'s sessions are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// A child session, tethered to the session that spawned it.
    Child,
    /// Work handed into a successor chain: the chain's first link spawning
    /// its second.
    Handoff,
    /// One link of a successor chain to the next, after the handoff into it.
    Chain,
}

/// The result of walking the lineage once: every node's [`Entry`] for the
/// list, and the [`Edge`]s for the map.
pub(crate) struct Lineage {
    pub entries: Vec<Entry>,
    pub edges: Vec<Edge>,
}

/// Every node's [`Entry`], each with its nested sessions and chain links in
/// draw order: a session, then its children (oldest first,
/// recursively), then the next link of its chain. A spawn loop, which no
/// real lineage has, is cut where it closes, so the walk always ends.
#[cfg(test)]
pub(crate) fn entries(nodes: &[Node<'_>]) -> Vec<Entry> {
    walk(nodes).entries
}

/// Walk `nodes`' lineage into list [`Entry`]s (see [`entries`]) and map
/// [`Edge`]s.
pub(crate) fn walk(nodes: &[Node<'_>]) -> Lineage {
    let by_id: HashMap<SessionId, Node<'_>> = nodes.iter().map(|n| (n.id, *n)).collect();
    let mut attached: HashMap<SessionId, Attach> =
        nodes.iter().map(|n| (n.id, attach(n, &by_id))).collect();
    cut_loops(&mut attached);
    let edges = edges(&attached);
    list_chains_in_place(&mut attached, &by_id);

    let mut kids: HashMap<SessionId, Vec<Kid>> = HashMap::new();
    for (&id, a) in &attached {
        if let Attach::Under { parent, nesting } = *a {
            kids.entry(parent).or_default().push(Kid { id, nesting });
        }
    }
    for list in kids.values_mut() {
        list.sort_unstable_by_key(|kid| kid.id);
    }

    let mut roots: Vec<SessionId> = attached
        .iter()
        .filter(|(_, a)| **a == Attach::Top)
        .map(|(&id, _)| id)
        .collect();
    roots.sort_unstable();
    let entries = roots
        .into_iter()
        .map(|root| entry(root, &kids, &by_id))
        .collect();
    Lineage { entries, edges }
}

/// The map's edge for each attached session, sorted so the list is stable
/// across walks. A chain link whose spawner is itself a link continues the
/// chain; one whose spawner isn't is the handoff into it.
fn edges(attached: &HashMap<SessionId, Attach>) -> Vec<Edge> {
    let mut edges: Vec<Edge> = attached
        .iter()
        .filter_map(|(&to, a)| {
            let Attach::Under { parent, nesting } = *a else {
                return None;
            };
            let kind = match nesting {
                Nesting::Child => EdgeKind::Child,
                Nesting::Link { .. } => match attached.get(&parent) {
                    Some(Attach::Under {
                        nesting: Nesting::Link { .. },
                        ..
                    }) => EdgeKind::Chain,
                    _ => EdgeKind::Handoff,
                },
                Nesting::Top => return None,
            };
            Some(Edge {
                from: parent,
                to,
                kind,
            })
        })
        .collect();
    edges.sort_unstable_by_key(|e| e.to);
    edges
}

/// Make every chain link spawned from another workspace (host or cwd)
/// top-level, for the list only: a chain block moved sessions out of the
/// worktree they run in, which read as if they ran here. The link starts
/// its own chain, or stands alone, in its own workspace's folder. The map
/// keeps the edge, since there each workspace is drawn where it is.
fn list_chains_in_place(
    attached: &mut HashMap<SessionId, Attach>,
    nodes: &HashMap<SessionId, Node<'_>>,
) {
    let workspace = |id: &SessionId| nodes.get(id).map(|n| n.workspace);
    for (id, a) in attached.iter_mut() {
        let Attach::Under {
            parent,
            nesting: Nesting::Link { .. },
        } = *a
        else {
            continue;
        };
        if workspace(id) != workspace(&parent) {
            *a = Attach::Top;
        }
    }
}

/// Make every node whose spawner chain leads back to itself top-level.
fn cut_loops(attached: &mut HashMap<SessionId, Attach>) {
    let mut ids: Vec<SessionId> = attached.keys().copied().collect();
    ids.sort_unstable();
    let limit = ids.len();
    for id in ids {
        let mut at = id;
        for _ in 0..=limit {
            let Some(Attach::Under { parent, .. }) = attached.get(&at) else {
                break;
            };
            at = *parent;
            if at == id {
                attached.insert(id, Attach::Top);
                break;
            }
        }
    }
}

fn entry(
    root: SessionId,
    kids: &HashMap<SessionId, Vec<Kid>>,
    nodes: &HashMap<SessionId, Node<'_>>,
) -> Entry {
    let links_of = |id: SessionId| {
        kids.get(&id)
            .into_iter()
            .flatten()
            .filter(|kid| matches!(kid.nesting, Nesting::Link { .. }))
    };
    let mut out = Entry {
        anchor: root,
        ids: Vec::new(),
        places: Vec::new(),
        chain: None,
    };
    if links_of(root).next().is_none() {
        push_with_nested(&mut out, root, Nesting::Top, 0, kids);
        return out;
    }

    // A chain: walk its links depth-first, oldest successor first.
    let mut chain = ChainShape {
        links: 0,
        tail: 0,
        epic: None,
    };
    let mut stack = vec![root];
    while let Some(link) = stack.pop() {
        let handed_on = links_of(link).next().is_some();
        chain.links += 1;
        chain.tail = out.ids.len();
        out.anchor = link;
        if chain.epic.is_none() {
            chain.epic = nodes
                .get(&link)
                .and_then(|n| n.card_parent)
                .map(str::to_owned);
        }
        push_with_nested(&mut out, link, Nesting::Link { handed_on }, 0, kids);
        let mut next: Vec<SessionId> = links_of(link).map(|kid| kid.id).collect();
        next.reverse();
        stack.extend(next);
    }
    out.chain = Some(chain);
    out
}

/// Push `id` at `depth`, then its children under it. A chain
/// link spawned from a nested session isn't part of the top-level chain, so
/// it nests under its spawner like a child.
fn push_with_nested(
    out: &mut Entry,
    id: SessionId,
    nesting: Nesting,
    depth: u8,
    kids: &HashMap<SessionId, Vec<Kid>>,
) {
    out.ids.push(id);
    out.places.push(Place { nesting, depth });
    let nested = kids.get(&id).into_iter().flatten();
    for &Kid {
        id: kid,
        nesting: kid_nesting,
    } in nested
    {
        let kid_nesting = match (kid_nesting, depth, nesting) {
            // A link off a top-level link is the chain's, walked by `entry`.
            (Nesting::Link { .. }, 0, Nesting::Link { .. } | Nesting::Top) => continue,
            (Nesting::Link { .. }, ..) => Nesting::Child,
            (n, ..) => n,
        };
        push_with_nested(out, kid, kid_nesting, depth.saturating_add(1), kids);
    }
}

/// Each session's `agentium:` ref, kept between regroups so resolving a
/// `parent-session` doesn't format every session's ref each time the groups
/// are rebuilt.
#[derive(Default)]
pub(crate) struct SessionRefs {
    by_session: HashMap<SessionId, KnownRef>,
    /// Bumped by each [`sync`](Self::sync), which stamps what it saw with it.
    generation: u64,
}

/// One session's ref, the event id it was made from, and the last
/// [`SessionRefs::sync`] that saw the session.
struct KnownRef {
    event_id: String,
    session_ref: String,
    seen: u64,
}

impl SessionRefs {
    /// Bring the kept refs up to date with `sessions` (id, event id) and
    /// drop the refs of sessions no longer there. Only a new session, or one
    /// whose event id changed, formats a ref.
    pub fn sync<'a>(&mut self, sessions: impl Iterator<Item = (SessionId, &'a str)>) {
        self.generation += 1;
        let generation = self.generation;
        let mut seen = 0;
        for (id, event_id) in sessions {
            seen += 1;
            let known = self.by_session.entry(id).or_insert_with(|| KnownRef {
                event_id: String::new(),
                session_ref: String::new(),
                seen: generation,
            });
            known.seen = generation;
            if known.event_id != event_id || known.session_ref.is_empty() {
                known.event_id = event_id.to_owned();
                known.session_ref = agentium_core::wordid::session_ref(event_id);
            }
        }
        if seen != self.by_session.len() {
            self.by_session.retain(|_, known| known.seen == generation);
        }
    }

    /// The session whose ref is `session_ref`.
    pub fn find(&self, session_ref: &str) -> Option<SessionId> {
        self.by_session
            .iter()
            .find(|(_, known)| known.session_ref == session_ref)
            .map(|(&id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPIC: &str = "headway:dave/epic-epic-epic";

    fn node(id: SessionId) -> Node<'static> {
        Node {
            id,
            spawner: None,
            relation: SessionRelation::None,
            workspace: Workspace::default(),
            card: None,
            card_parent: None,
        }
    }

    fn on_card(id: SessionId, card: &'static str, parent: Option<&'static str>) -> Node<'static> {
        Node {
            card: Some(card),
            card_parent: parent,
            ..node(id)
        }
    }

    fn spawned(mut n: Node<'static>, by: SessionId, relation: SessionRelation) -> Node<'static> {
        n.spawner = Some(by);
        n.relation = relation;
        n
    }

    /// One drawn row of an [`Entry`], for comparing whole entries.
    #[derive(Debug, PartialEq)]
    struct Drawn {
        id: SessionId,
        nesting: Nesting,
        depth: u8,
    }

    fn drawn(id: SessionId, nesting: Nesting, depth: u8) -> Drawn {
        Drawn { id, nesting, depth }
    }

    fn nestings(e: &Entry) -> Vec<Drawn> {
        e.ids
            .iter()
            .zip(&e.places)
            .map(|(&id, p)| drawn(id, p.nesting, p.depth))
            .collect()
    }

    #[test]
    fn an_explicit_successor_is_a_link_whatever_its_cards() {
        let a = node(1);
        let b = spawned(node(2), 1, SessionRelation::Successor);
        assert!(is_chain_link(&b, &a));
    }

    #[test]
    fn sibling_cards_under_one_parent_infer_a_link_from_a_plain_spawn() {
        let a = on_card(1, "headway:dave/a", Some(EPIC));
        let b = spawned(
            on_card(2, "headway:dave/b", Some(EPIC)),
            1,
            SessionRelation::None,
        );
        assert!(is_chain_link(&b, &a));
    }

    #[test]
    fn a_plain_spawn_without_sibling_cards_is_no_link() {
        let a = on_card(1, "headway:dave/a", Some(EPIC));
        let same_card = spawned(
            on_card(2, "headway:dave/a", Some(EPIC)),
            1,
            SessionRelation::None,
        );
        let other_epic = spawned(
            on_card(3, "headway:dave/b", Some("headway:dave/other")),
            1,
            SessionRelation::None,
        );
        let no_parent = spawned(on_card(4, "headway:dave/b", None), 1, SessionRelation::None);
        let no_card = spawned(node(5), 1, SessionRelation::None);
        for b in [same_card, other_epic, no_parent, no_card] {
            assert!(!is_chain_link(&b, &a), "{b:?}");
        }
    }

    #[test]
    fn a_child_is_never_a_link_even_on_a_sibling_card() {
        let a = on_card(1, "headway:dave/a", Some(EPIC));
        let b = spawned(
            on_card(2, "headway:dave/b", Some(EPIC)),
            1,
            SessionRelation::Child,
        );
        assert!(!is_chain_link(&b, &a));
        let e = entries(&[a, b]);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].chain, None);
        assert_eq!(
            nestings(&e[0]),
            vec![drawn(1, Nesting::Top, 0), drawn(2, Nesting::Child, 1)]
        );
    }

    #[test]
    fn children_nest_under_their_parent_oldest_first() {
        let parent = node(1);
        let child_b = spawned(node(4), 1, SessionRelation::Child);
        let child_a = spawned(node(2), 1, SessionRelation::Child);
        let grandchild = spawned(node(5), 2, SessionRelation::Child);
        let loner = node(6);
        let e = entries(&[child_b, loner, parent, grandchild, child_a]);
        assert_eq!(e.len(), 2);
        assert_eq!(
            nestings(&e[0]),
            vec![
                drawn(1, Nesting::Top, 0),
                drawn(2, Nesting::Child, 1),
                drawn(5, Nesting::Child, 2),
                drawn(4, Nesting::Child, 1),
            ]
        );
        assert_eq!(e[0].anchor, 1);
        assert_eq!(nestings(&e[1]), vec![drawn(6, Nesting::Top, 0)]);
    }

    #[test]
    fn a_four_link_chain_walks_in_order_and_anchors_at_its_tail() {
        let cards = ["headway:dave/a", "headway:dave/b", "headway:dave/c"];
        // Link 1 → 2 explicit; 2 → 3 inferred; 3 → 4 explicit. Link 2 has a
        // child of its own, which stays under it.
        let n1 = on_card(1, cards[0], Some(EPIC));
        let n2 = spawned(
            on_card(2, cards[1], Some(EPIC)),
            1,
            SessionRelation::Successor,
        );
        let sub = spawned(node(9), 2, SessionRelation::Child);
        let n3 = spawned(on_card(3, cards[2], Some(EPIC)), 2, SessionRelation::None);
        let n4 = spawned(node(4), 3, SessionRelation::Successor);
        let e = entries(&[n4, n3, sub, n2, n1]);
        assert_eq!(e.len(), 1);
        let chain = e[0].chain.as_ref().expect("a chain");
        assert_eq!(chain.links, 4);
        assert_eq!(chain.epic.as_deref(), Some(EPIC));
        assert_eq!(chain.tail, 4);
        assert_eq!(e[0].anchor, 4);
        let done = Nesting::Link { handed_on: true };
        assert_eq!(
            nestings(&e[0]),
            vec![
                drawn(1, done, 0),
                drawn(2, done, 0),
                drawn(9, Nesting::Child, 1),
                drawn(3, done, 0),
                drawn(4, Nesting::Link { handed_on: false }, 0),
            ]
        );
    }

    /// The map draws the handoff into a chain, then chain links between its
    /// later links, a tether to a child, and nothing for a plain spawn.
    #[test]
    fn edges_name_the_handoff_chain_links_and_children() {
        let cards = ["headway:dave/a", "headway:dave/b", "headway:dave/c"];
        let n1 = on_card(1, cards[0], Some(EPIC));
        let n2 = spawned(
            on_card(2, cards[1], Some(EPIC)),
            1,
            SessionRelation::Successor,
        );
        // Inferred: sibling cards under one epic.
        let n3 = spawned(on_card(3, cards[2], Some(EPIC)), 2, SessionRelation::None);
        let child = spawned(node(5), 3, SessionRelation::Child);
        let plain = spawned(node(7), 1, SessionRelation::None);
        let edges = walk(&[plain, child, n3, n2, n1]).edges;
        let edge = |from, to, kind| Edge { from, to, kind };
        assert_eq!(
            edges,
            vec![
                edge(1, 2, EdgeKind::Handoff),
                edge(2, 3, EdgeKind::Chain),
                edge(3, 5, EdgeKind::Child),
            ]
        );
    }

    /// A chain handed to another worktree, then to another host, splits in
    /// the list: each workspace lists the links that ran in it, so no
    /// session moves out of its own folder. The map keeps every handoff.
    #[test]
    fn a_chain_in_the_list_keeps_each_link_in_its_own_workspace() {
        use std::path::Path;
        let at = |n: Node<'static>, host, cwd: &'static str| Node {
            workspace: Workspace {
                host,
                cwd: Some(Path::new(cwd)),
            },
            ..n
        };
        let succ = |id, by| spawned(node(id), by, SessionRelation::Successor);
        let n1 = at(node(1), "monad", "/home/dev/notedeck");
        let n2 = at(succ(2, 1), "monad", "/home/dev/notedeck");
        let n3 = at(succ(3, 2), "monad", "/home/dev/notedeck-scene");
        let n4 = at(succ(4, 3), "hydra", "/home/dev/notedeck-scene");
        let n5 = at(succ(5, 4), "hydra", "/home/dev/notedeck-scene");
        let walked = walk(&[n5, n4, n3, n2, n1]);

        let ids: Vec<Vec<SessionId>> = walked.entries.iter().map(|e| e.ids.clone()).collect();
        assert_eq!(ids, vec![vec![1, 2], vec![3], vec![4, 5]]);
        let chains: Vec<bool> = walked.entries.iter().map(|e| e.chain.is_some()).collect();
        assert_eq!(chains, vec![true, false, true], "a lone link is no chain");

        let edge = |from, to, kind| Edge { from, to, kind };
        assert_eq!(
            walked.edges,
            vec![
                edge(1, 2, EdgeKind::Handoff),
                edge(2, 3, EdgeKind::Chain),
                edge(3, 4, EdgeKind::Chain),
                edge(4, 5, EdgeKind::Chain),
            ],
            "the map still draws every handoff"
        );
    }

    #[test]
    fn a_spawner_that_isnt_known_here_leaves_the_session_on_top() {
        let orphan = spawned(node(2), 99, SessionRelation::Child);
        let e = entries(&[orphan]);
        assert_eq!(nestings(&e[0]), vec![drawn(2, Nesting::Top, 0)]);
    }

    #[test]
    fn a_spawn_loop_is_cut_and_every_session_still_listed() {
        let a = spawned(node(1), 2, SessionRelation::Child);
        let b = spawned(node(2), 1, SessionRelation::Child);
        let e = entries(&[a, b]);
        let listed: usize = e.iter().map(|e| e.ids.len()).sum();
        assert_eq!(listed, 2);
    }

    #[test]
    fn session_refs_find_a_session_by_its_ref_and_follow_event_id_changes() {
        let mut refs = SessionRefs::default();
        refs.sync([(1, "one"), (2, "two")].into_iter());
        let two = agentium_core::wordid::session_ref("two");
        assert_eq!(refs.find(&two), Some(2));
        refs.sync([(1, "one"), (2, "renamed")].into_iter());
        assert_eq!(refs.find(&two), None);
        let renamed = agentium_core::wordid::session_ref("renamed");
        assert_eq!(refs.find(&renamed), Some(2));
        refs.sync([(2, "renamed")].into_iter());
        assert_eq!(refs.find(&agentium_core::wordid::session_ref("one")), None);
        assert_eq!(refs.find(&renamed), Some(2), "kept across the shrink");
    }
}
