use glam::{Mat4, Quat, Vec3};

use crate::camera::Camera;
use crate::model::Model;

/// A unique handle for a node in the scene graph.
/// Uses arena index + generation to prevent stale handle reuse.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeId {
    pub index: u32,
    pub generation: u32,
}

/// Backward-compatible alias for existing code that uses ObjectId.
pub type ObjectId = NodeId;

/// Transform for a scene node (position, rotation, scale).
#[derive(Clone, Debug)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn from_translation(t: Vec3) -> Self {
        Self {
            translation: t,
            ..Default::default()
        }
    }

    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

/// A node in the scene graph.
pub struct Node {
    /// Local transform relative to parent (or world if root).
    pub local: Transform,

    /// Cached world-space matrix. Valid when `dirty == false`.
    world_matrix: Mat4,

    /// When true, world_matrix needs recomputation.
    dirty: bool,

    /// Generation for this slot (matches NodeId.generation when alive).
    generation: u32,

    /// Parent node. None means this is a root node.
    parent: Option<NodeId>,

    /// First child (intrusive linked list through siblings).
    first_child: Option<NodeId>,

    /// Next sibling in parent's child list.
    next_sibling: Option<NodeId>,

    /// If Some, this node is renderable with the given Model handle.
    /// If None, this is a grouping/transform-only node.
    pub model: Option<Model>,

    /// Whether this slot is occupied.
    alive: bool,
}

impl Node {
    /// Get the cached world-space matrix.
    /// Only valid after `update_world_transforms()`.
    pub fn world_matrix(&self) -> Mat4 {
        self.world_matrix
    }
}

pub struct World {
    pub camera: Camera,

    /// Arena of all nodes.
    nodes: Vec<Node>,

    /// Free slot indices for reuse.
    free_list: Vec<u32>,

    /// Cached list of NodeIds that have a Model (renderable).
    /// Rebuilt when renderables_dirty is true.
    renderables: Vec<NodeId>,

    /// True when renderables list needs rebuilding.
    renderables_dirty: bool,

    pub selected_object: Option<NodeId>,
}

impl World {
    pub fn new(camera: Camera) -> Self {
        Self {
            camera,
            nodes: Vec::new(),
            free_list: Vec::new(),
            renderables: Vec::new(),
            renderables_dirty: false,
            selected_object: None,
        }
    }

    // ── Arena internals ──────────────────────────────────────────

    fn alloc_slot(&mut self) -> (u32, u32) {
        if let Some(index) = self.free_list.pop() {
            let node = &mut self.nodes[index as usize];
            node.generation += 1;
            node.alive = true;
            node.dirty = true;
            node.parent = None;
            node.first_child = None;
            node.next_sibling = None;
            node.model = None;
            node.world_matrix = Mat4::IDENTITY;
            (index, node.generation)
        } else {
            let index = self.nodes.len() as u32;
            self.nodes.push(Node {
                local: Transform::default(),
                world_matrix: Mat4::IDENTITY,
                dirty: true,
                generation: 0,
                parent: None,
                first_child: None,
                next_sibling: None,
                model: None,
                alive: true,
            });
            (index, 0)
        }
    }

    fn is_valid(&self, id: NodeId) -> bool {
        let idx = id.index as usize;
        idx < self.nodes.len()
            && self.nodes[idx].alive
            && self.nodes[idx].generation == id.generation
    }

    fn mark_dirty(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(nid) = stack.pop() {
            let node = &mut self.nodes[nid.index as usize];
            if node.dirty {
                continue;
            }
            node.dirty = true;
            let mut child = node.first_child;
            while let Some(c) = child {
                stack.push(c);
                child = self.nodes[c.index as usize].next_sibling;
            }
        }
    }

    fn attach_child(&mut self, parent: NodeId, child: NodeId) {
        let old_first = self.nodes[parent.index as usize].first_child;
        self.nodes[child.index as usize].next_sibling = old_first;
        self.nodes[parent.index as usize].first_child = Some(child);
    }

    fn detach_child(&mut self, parent: NodeId, child: NodeId) {
        let first = self.nodes[parent.index as usize].first_child;
        if first == Some(child) {
            self.nodes[parent.index as usize].first_child =
                self.nodes[child.index as usize].next_sibling;
        } else {
            let mut prev = first;
            while let Some(p) = prev {
                let next = self.nodes[p.index as usize].next_sibling;
                if next == Some(child) {
                    self.nodes[p.index as usize].next_sibling =
                        self.nodes[child.index as usize].next_sibling;
                    break;
                }
                prev = next;
            }
        }
        self.nodes[child.index as usize].next_sibling = None;
    }

    fn is_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = Some(node);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.nodes[c.index as usize].parent;
        }
        false
    }

    // ── Public scene graph API ───────────────────────────────────

    /// Create a grouping node (no model) with an optional parent.
    pub fn create_node(&mut self, local: Transform, parent: Option<NodeId>) -> NodeId {
        let (index, generation) = self.alloc_slot();
        self.nodes[index as usize].local = local;

        let id = NodeId { index, generation };

        if let Some(p) = parent {
            if self.is_valid(p) {
                self.nodes[index as usize].parent = Some(p);
                self.attach_child(p, id);
            }
        }

        id
    }

    /// Create a renderable node with a Model and optional parent.
    pub fn create_renderable(
        &mut self,
        model: Model,
        local: Transform,
        parent: Option<NodeId>,
    ) -> NodeId {
        let id = self.create_node(local, parent);
        self.nodes[id.index as usize].model = Some(model);
        self.renderables_dirty = true;
        id
    }

    /// Remove a node and all its descendants.
    pub fn remove_node(&mut self, id: NodeId) -> bool {
        if !self.is_valid(id) {
            return false;
        }

        // Collect all nodes in the subtree
        let mut to_remove = Vec::new();
        let mut stack = vec![id];
        while let Some(nid) = stack.pop() {
            to_remove.push(nid);
            let mut child = self.nodes[nid.index as usize].first_child;
            while let Some(c) = child {
                stack.push(c);
                child = self.nodes[c.index as usize].next_sibling;
            }
        }

        // Detach root of subtree from its parent
        if let Some(parent_id) = self.nodes[id.index as usize].parent {
            self.detach_child(parent_id, id);
        }

        // Free all collected nodes
        for nid in &to_remove {
            let node = &mut self.nodes[nid.index as usize];
            node.alive = false;
            node.first_child = None;
            node.next_sibling = None;
            node.parent = None;
            node.model = None;
            self.free_list.push(nid.index);
        }

        self.renderables_dirty = true;
        true
    }

    /// Set a node's local transform. Marks it and descendants dirty.
    pub fn set_local_transform(&mut self, id: NodeId, local: Transform) -> bool {
        if !self.is_valid(id) {
            return false;
        }
        self.nodes[id.index as usize].local = local;
        self.mark_dirty(id);
        true
    }

    /// Reparent a node. Pass None to make it a root node.
    pub fn set_parent(&mut self, id: NodeId, new_parent: Option<NodeId>) -> bool {
        if !self.is_valid(id) {
            return false;
        }
        if let Some(p) = new_parent {
            if !self.is_valid(p) {
                return false;
            }
            if self.is_ancestor(id, p) {
                return false;
            }
        }

        // Detach from old parent
        if let Some(old_parent) = self.nodes[id.index as usize].parent {
            self.detach_child(old_parent, id);
        }

        // Attach to new parent
        self.nodes[id.index as usize].parent = new_parent;
        if let Some(p) = new_parent {
            self.attach_child(p, id);
        }

        self.mark_dirty(id);
        true
    }

    /// Attach or detach a Model on an existing node.
    pub fn set_model(&mut self, id: NodeId, model: Option<Model>) -> bool {
        if !self.is_valid(id) {
            return false;
        }
        self.nodes[id.index as usize].model = model;
        self.renderables_dirty = true;
        true
    }

    /// Get the cached world matrix for a node.
    pub fn world_matrix(&self, id: NodeId) -> Option<Mat4> {
        if !self.is_valid(id) {
            return None;
        }
        Some(self.nodes[id.index as usize].world_matrix)
    }

    /// Get a node's local transform.
    pub fn local_transform(&self, id: NodeId) -> Option<&Transform> {
        if !self.is_valid(id) {
            return None;
        }
        Some(&self.nodes[id.index as usize].local)
    }

    /// Get a node by id.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        if !self.is_valid(id) {
            return None;
        }
        Some(&self.nodes[id.index as usize])
    }

    /// Iterate renderable node ids (nodes with a Model).
    pub fn renderables(&self) -> &[NodeId] {
        &self.renderables
    }

    /// Recompute world matrices for all dirty nodes. Call once per frame.
    pub fn update_world_transforms(&mut self) {
        // Rebuild renderables list if needed
        if self.renderables_dirty {
            self.renderables.clear();
            for (i, node) in self.nodes.iter().enumerate() {
                if node.alive && node.model.is_some() {
                    self.renderables.push(NodeId {
                        index: i as u32,
                        generation: node.generation,
                    });
                }
            }
            self.renderables_dirty = false;
        }

        // Process root nodes (no parent) and recurse into children
        for i in 0..self.nodes.len() {
            let node = &self.nodes[i];
            if !node.alive || !node.dirty || node.parent.is_some() {
                continue;
            }
            self.nodes[i].world_matrix = self.nodes[i].local.to_matrix();
            self.nodes[i].dirty = false;
            self.update_children(i);
        }

        // Second pass: catch any remaining dirty nodes (reparented mid-frame)
        for i in 0..self.nodes.len() {
            if self.nodes[i].alive && self.nodes[i].dirty {
                self.recompute_world_matrix(i);
            }
        }
    }

    fn update_children(&mut self, parent_idx: usize) {
        let parent_world = self.nodes[parent_idx].world_matrix;
        let mut child_id = self.nodes[parent_idx].first_child;
        while let Some(cid) = child_id {
            let ci = cid.index as usize;
            if self.nodes[ci].alive {
                let local = self.nodes[ci].local.to_matrix();
                self.nodes[ci].world_matrix = parent_world * local;
                self.nodes[ci].dirty = false;
                self.update_children(ci);
            }
            child_id = self.nodes[ci].next_sibling;
        }
    }

    fn recompute_world_matrix(&mut self, index: usize) {
        // Build chain from this node up to root
        let mut chain = Vec::with_capacity(8);
        let mut cur = index;
        loop {
            chain.push(cur);
            match self.nodes[cur].parent {
                Some(p) if self.nodes[p.index as usize].alive => {
                    cur = p.index as usize;
                }
                _ => break,
            }
        }

        // Walk from root down to target
        chain.reverse();
        let mut parent_world = Mat4::IDENTITY;
        for &idx in &chain {
            let node = &self.nodes[idx];
            if !node.dirty {
                parent_world = node.world_matrix;
                continue;
            }
            let world = parent_world * node.local.to_matrix();
            self.nodes[idx].world_matrix = world;
            self.nodes[idx].dirty = false;
            parent_world = world;
        }
    }

    // ── Backward-compatible API ──────────────────────────────────

    /// Legacy: place a renderable object as a root node.
    pub fn add_object(&mut self, model: Model, transform: Transform) -> ObjectId {
        self.create_renderable(model, transform, None)
    }

    /// Legacy: remove an object.
    pub fn remove_object(&mut self, id: ObjectId) -> bool {
        self.remove_node(id)
    }

    /// Legacy: update an object's transform.
    pub fn update_transform(&mut self, id: ObjectId, transform: Transform) -> bool {
        self.set_local_transform(id, transform)
    }

    /// Legacy: get a node by object id.
    pub fn get_object(&self, id: ObjectId) -> Option<&Node> {
        self.get_node(id)
    }

    /// Number of renderable objects in the scene.
    pub fn num_objects(&self) -> usize {
        self.renderables.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;
    use glam::Vec3;

    fn test_world() -> World {
        World::new(Camera::new(Vec3::new(0.0, 2.0, 5.0), Vec3::ZERO))
    }

    fn model(id: u64) -> Model {
        Model { id }
    }

    // ── Arena basics ──────────────────────────────────────────────

    #[test]
    fn create_node_returns_valid_id() {
        let mut w = test_world();
        let id = w.create_node(Transform::default(), None);
        assert!(w.is_valid(id));
        assert!(w.get_node(id).is_some());
    }

    #[test]
    fn create_renderable_appears_in_renderables() {
        let mut w = test_world();
        let id = w.create_renderable(model(1), Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.renderables().len(), 1);
        assert_eq!(w.renderables()[0], id);
    }

    #[test]
    fn grouping_node_not_in_renderables() {
        let mut w = test_world();
        w.create_node(Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.renderables().len(), 0);
    }

    #[test]
    fn multiple_renderables() {
        let mut w = test_world();
        let a = w.create_renderable(model(1), Transform::default(), None);
        let b = w.create_renderable(model(2), Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 2);
        let ids = w.renderables();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
    }

    // ── Removal and free list ─────────────────────────────────────

    #[test]
    fn remove_node_invalidates_id() {
        let mut w = test_world();
        let id = w.create_renderable(model(1), Transform::default(), None);
        assert!(w.remove_node(id));
        assert!(!w.is_valid(id));
        assert!(w.get_node(id).is_none());
    }

    #[test]
    fn remove_node_clears_renderables() {
        let mut w = test_world();
        let id = w.create_renderable(model(1), Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 1);
        w.remove_node(id);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 0);
    }

    #[test]
    fn stale_handle_after_reuse() {
        let mut w = test_world();
        let old = w.create_node(Transform::default(), None);
        w.remove_node(old);
        // Allocate a new node, which should reuse the slot with bumped generation
        let new = w.create_node(Transform::default(), None);
        assert_eq!(old.index, new.index);
        assert_ne!(old.generation, new.generation);
        // Old handle must be invalid
        assert!(!w.is_valid(old));
        assert!(w.is_valid(new));
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut w = test_world();
        let fake = NodeId {
            index: 99,
            generation: 0,
        };
        assert!(!w.remove_node(fake));
    }

    // ── Parent-child relationships ────────────────────────────────

    #[test]
    fn create_with_parent() {
        let mut w = test_world();
        let parent = w.create_node(Transform::default(), None);
        let child = w.create_node(Transform::default(), Some(parent));
        let parent_node = w.get_node(parent).unwrap();
        assert_eq!(parent_node.first_child, Some(child));
    }

    #[test]
    fn reparent_node() {
        let mut w = test_world();
        let a = w.create_node(Transform::default(), None);
        let b = w.create_node(Transform::default(), None);
        let child = w.create_node(Transform::default(), Some(a));

        // Child is under a
        assert_eq!(w.get_node(a).unwrap().first_child, Some(child));

        // Reparent to b
        assert!(w.set_parent(child, Some(b)));
        assert!(w.get_node(a).unwrap().first_child.is_none());
        assert_eq!(w.get_node(b).unwrap().first_child, Some(child));
    }

    #[test]
    fn reparent_to_none_makes_root() {
        let mut w = test_world();
        let parent = w.create_node(Transform::default(), None);
        let child = w.create_node(Transform::default(), Some(parent));
        assert!(w.set_parent(child, None));
        assert!(w.get_node(parent).unwrap().first_child.is_none());
    }

    #[test]
    fn cycle_prevention() {
        let mut w = test_world();
        let a = w.create_node(Transform::default(), None);
        let b = w.create_node(Transform::default(), Some(a));
        let c = w.create_node(Transform::default(), Some(b));

        // Trying to make a a child of c should fail (c -> b -> a cycle)
        assert!(!w.set_parent(a, Some(c)));

        // Trying to make a a child of b should also fail
        assert!(!w.set_parent(a, Some(b)));

        // Self-parenting should fail
        assert!(!w.set_parent(a, Some(a)));
    }

    #[test]
    fn remove_subtree() {
        let mut w = test_world();
        let root = w.create_node(Transform::default(), None);
        let child = w.create_renderable(model(1), Transform::default(), Some(root));
        let grandchild = w.create_renderable(model(2), Transform::default(), Some(child));

        w.remove_node(root);

        assert!(!w.is_valid(root));
        assert!(!w.is_valid(child));
        assert!(!w.is_valid(grandchild));
    }

    #[test]
    fn remove_child_detaches_from_parent() {
        let mut w = test_world();
        let parent = w.create_node(Transform::default(), None);
        let c1 = w.create_node(Transform::default(), Some(parent));
        let c2 = w.create_node(Transform::default(), Some(parent));

        w.remove_node(c1);

        // Parent should still have c2
        assert!(w.is_valid(parent));
        assert!(w.is_valid(c2));
        let parent_node = w.get_node(parent).unwrap();
        assert_eq!(parent_node.first_child, Some(c2));
    }

    // ── Transform computation ─────────────────────────────────────

    #[test]
    fn root_world_matrix_equals_local() {
        let mut w = test_world();
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let expected = t.to_matrix();
        let id = w.create_node(t, None);
        w.update_world_transforms();
        assert_eq!(w.world_matrix(id).unwrap(), expected);
    }

    #[test]
    fn child_inherits_parent_transform() {
        let mut w = test_world();
        let parent_t = Transform::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let child_t = Transform::from_translation(Vec3::new(0.0, 5.0, 0.0));

        let parent = w.create_node(parent_t.clone(), None);
        let child = w.create_node(child_t.clone(), Some(parent));
        w.update_world_transforms();

        let expected = parent_t.to_matrix() * child_t.to_matrix();
        let actual = w.world_matrix(child).unwrap();

        // Check that the child's world position is (10, 5, 0)
        let pos = actual.col(3);
        assert!((pos.x - 10.0).abs() < 1e-5);
        assert!((pos.y - 5.0).abs() < 1e-5);
        assert!((pos.z - 0.0).abs() < 1e-5);
        assert_eq!(actual, expected);
    }

    #[test]
    fn grandchild_transform_chain() {
        let mut w = test_world();
        let t1 = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
        let t2 = Transform::from_translation(Vec3::new(0.0, 2.0, 0.0));
        let t3 = Transform::from_translation(Vec3::new(0.0, 0.0, 3.0));

        let a = w.create_node(t1.clone(), None);
        let b = w.create_node(t2.clone(), Some(a));
        let c = w.create_node(t3.clone(), Some(b));
        w.update_world_transforms();

        let world_c = w.world_matrix(c).unwrap();
        let pos = world_c.col(3);
        assert!((pos.x - 1.0).abs() < 1e-5);
        assert!((pos.y - 2.0).abs() < 1e-5);
        assert!((pos.z - 3.0).abs() < 1e-5);
    }

    // ── Dirty flag propagation ────────────────────────────────────

    #[test]
    fn moving_parent_updates_children() {
        let mut w = test_world();
        let parent = w.create_node(Transform::from_translation(Vec3::X), None);
        let child = w.create_node(Transform::from_translation(Vec3::Y), Some(parent));
        w.update_world_transforms();

        // Verify initial position
        let pos = w.world_matrix(child).unwrap().col(3);
        assert!((pos.x - 1.0).abs() < 1e-5);
        assert!((pos.y - 1.0).abs() < 1e-5);

        // Move parent
        w.set_local_transform(
            parent,
            Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
        );
        w.update_world_transforms();

        // Child should now be at (5, 1, 0)
        let pos = w.world_matrix(child).unwrap().col(3);
        assert!((pos.x - 5.0).abs() < 1e-5);
        assert!((pos.y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn set_local_transform_invalid_id() {
        let mut w = test_world();
        let fake = NodeId {
            index: 0,
            generation: 99,
        };
        assert!(!w.set_local_transform(fake, Transform::default()));
    }

    // ── set_model ─────────────────────────────────────────────────

    #[test]
    fn attach_model_to_grouping_node() {
        let mut w = test_world();
        let id = w.create_node(Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 0);

        w.set_model(id, Some(model(42)));
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 1);
    }

    #[test]
    fn detach_model_from_renderable() {
        let mut w = test_world();
        let id = w.create_renderable(model(1), Transform::default(), None);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 1);

        w.set_model(id, None);
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 0);
        // Node still valid, just no longer renderable
        assert!(w.is_valid(id));
    }

    // ── Backward-compatible API ───────────────────────────────────

    #[test]
    fn legacy_add_remove_object() {
        let mut w = test_world();
        let id = w.add_object(model(1), Transform::from_translation(Vec3::Z));
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 1);
        assert!(w.get_object(id).is_some());

        assert!(w.remove_object(id));
        w.update_world_transforms();
        assert_eq!(w.num_objects(), 0);
    }

    #[test]
    fn legacy_update_transform() {
        let mut w = test_world();
        let id = w.add_object(model(1), Transform::from_translation(Vec3::ZERO));
        w.update_world_transforms();

        let new_t = Transform::from_translation(Vec3::new(7.0, 8.0, 9.0));
        assert!(w.update_transform(id, new_t));
        w.update_world_transforms();

        let pos = w.world_matrix(id).unwrap().col(3);
        assert!((pos.x - 7.0).abs() < 1e-5);
        assert!((pos.y - 8.0).abs() < 1e-5);
        assert!((pos.z - 9.0).abs() < 1e-5);
    }

    // ── Multiple siblings ─────────────────────────────────────────

    #[test]
    fn multiple_children_all_transform_correctly() {
        let mut w = test_world();
        let parent = w.create_node(Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)), None);
        let c1 = w.create_node(
            Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),
            Some(parent),
        );
        let c2 = w.create_node(
            Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)),
            Some(parent),
        );
        let c3 = w.create_node(
            Transform::from_translation(Vec3::new(3.0, 0.0, 0.0)),
            Some(parent),
        );
        w.update_world_transforms();

        assert!((w.world_matrix(c1).unwrap().col(3).x - 11.0).abs() < 1e-5);
        assert!((w.world_matrix(c2).unwrap().col(3).x - 12.0).abs() < 1e-5);
        assert!((w.world_matrix(c3).unwrap().col(3).x - 13.0).abs() < 1e-5);
    }

    #[test]
    fn remove_middle_sibling() {
        let mut w = test_world();
        let parent = w.create_node(Transform::default(), None);
        let c1 = w.create_node(Transform::default(), Some(parent));
        let c2 = w.create_node(Transform::default(), Some(parent));
        let c3 = w.create_node(Transform::default(), Some(parent));

        w.remove_node(c2);

        assert!(w.is_valid(c1));
        assert!(!w.is_valid(c2));
        assert!(w.is_valid(c3));

        // Parent should still link to c1 and c3
        // (linked list: c3 -> c1 after c2 removed, since prepend order is c3, c2, c1)
        let mut count = 0;
        let mut cur = w.get_node(parent).unwrap().first_child;
        while let Some(c) = cur {
            count += 1;
            cur = w.get_node(c).unwrap().next_sibling;
        }
        assert_eq!(count, 2);
    }

    // ── Scale and rotation ────────────────────────────────────────

    #[test]
    fn scaled_parent_affects_child_position() {
        let mut w = test_world();
        let parent_t = Transform {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(2.0),
        };
        let child_t = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));

        let parent = w.create_node(parent_t, None);
        let child = w.create_node(child_t, Some(parent));
        w.update_world_transforms();

        // Child at local (1,0,0) under 2x scale parent should be at world (2,0,0)
        let pos = w.world_matrix(child).unwrap().col(3);
        assert!((pos.x - 2.0).abs() < 1e-5);
    }

    // ── Reparent updates transforms ───────────────────────────────

    #[test]
    fn reparent_recomputes_world_matrix() {
        let mut w = test_world();
        let a = w.create_node(Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)), None);
        let b = w.create_node(Transform::from_translation(Vec3::new(20.0, 0.0, 0.0)), None);
        let child = w.create_node(
            Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),
            Some(a),
        );
        w.update_world_transforms();

        // Under a: world x = 11
        assert!((w.world_matrix(child).unwrap().col(3).x - 11.0).abs() < 1e-5);

        // Reparent to b
        w.set_parent(child, Some(b));
        w.update_world_transforms();

        // Under b: world x = 21
        assert!((w.world_matrix(child).unwrap().col(3).x - 21.0).abs() < 1e-5);
    }

    // ── Edge case: empty world ────────────────────────────────────

    #[test]
    fn empty_world_update_is_safe() {
        let mut w = test_world();
        w.update_world_transforms();
        assert_eq!(w.renderables().len(), 0);
    }

    #[test]
    fn world_matrix_invalid_id_returns_none() {
        let w = test_world();
        let fake = NodeId {
            index: 0,
            generation: 0,
        };
        assert!(w.world_matrix(fake).is_none());
    }
}
