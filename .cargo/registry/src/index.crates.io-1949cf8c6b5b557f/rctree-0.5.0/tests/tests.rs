extern crate rctree;

use rctree::{Node, NodeEdge};

use std::fmt;

#[test]
fn it_works() {
    use std::cell;

    struct DropTracker<'a>(&'a cell::Cell<u32>);
    impl<'a> Drop for DropTracker<'a> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let mut new_counter = 0;
    let drop_counter = cell::Cell::new(0);
    let mut new = || {
        new_counter += 1;
        Node::new((new_counter, DropTracker(&drop_counter)))
    };

    {
        let a = new(); // 1
        a.append(new()); // 2
        a.append(new()); // 3
        a.prepend(new()); // 4
        let b = new(); // 5
        b.append(a.clone());
        a.insert_before(new()); // 6
        a.insert_before(new()); // 7
        a.insert_after(new()); // 8
        a.insert_after(new()); // 9
        let c = new(); // 10
        b.append(c.clone());

        assert_eq!(drop_counter.get(), 0);
        c.previous_sibling().unwrap().detach();
        assert_eq!(drop_counter.get(), 1);

        assert_eq!(
            b.descendants()
                .map(|node| {
                    let borrow = node.borrow();
                    borrow.0
                })
                .collect::<Vec<_>>(),
            [5, 6, 7, 1, 4, 2, 3, 9, 10]
        );
    }

    assert_eq!(drop_counter.get(), 10);
}

struct TreePrinter<T>(Node<T>);

impl<T: fmt::Debug> fmt::Debug for TreePrinter<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "{:?}", self.0.borrow()).unwrap();
        iter_children(&self.0, 1, f);

        Ok(())
    }
}

fn iter_children<T: fmt::Debug>(parent: &Node<T>, depth: usize, f: &mut fmt::Formatter) {
    for child in parent.children() {
        for _ in 0..depth {
            write!(f, "    ").unwrap();
        }
        writeln!(f, "{:?}", child.borrow()).unwrap();
        iter_children(&child, depth + 1, f);
    }
}

#[test]
fn make_copy_1() {
    let node1 = Node::new(1);
    let node2 = Node::new(2);
    node1.append(node2);
    let node1_copy = node1.make_copy();
    node1.append(node1_copy);

    assert_eq!(
        format!("{:?}", TreePrinter(node1)),
        "1
    2
    1
"
    );
}

#[test]
fn make_deep_copy_1() {
    let node1 = Node::new(1);
    let node2 = Node::new(2);
    node1.append(node2.clone());
    node2.append(node1.make_deep_copy());

    assert_eq!(
        format!("{:?}", TreePrinter(node1)),
        "1
    2
        1
            2
"
    );
}

#[test]
#[should_panic]
fn append_1() {
    let node1 = Node::new(1);
    let node1_2 = node1.clone();
    node1.append(node1_2);
}

#[test]
#[should_panic]
fn prepend_1() {
    let node1 = Node::new(1);
    let node1_2 = node1.clone();
    node1.prepend(node1_2);
}

#[test]
#[should_panic]
fn insert_before_1() {
    let node1 = Node::new(1);
    let node1_2 = node1.clone();
    node1.insert_before(node1_2);
}

#[test]
#[should_panic]
fn insert_after_1() {
    let node1 = Node::new(1);
    let node1_2 = node1.clone();
    node1.insert_after(node1_2);
}

#[test]
#[should_panic]
fn iter_1() {
    let node1 = Node::new(1);
    let node2 = Node::new(2);
    node1.append(node2.clone());
    node2.append(node1.make_deep_copy());

    let _n = node2.borrow_mut();
    for _ in node1.descendants() {}
}

#[test]
fn stack_overflow() {
    let mut parent = Node::new(1);
    for _ in 0..200_000 {
        let node = Node::new(1);
        node.append(parent.clone());
        parent = node;
    }
}

#[test]
fn weak_1() {
    let node1 = Node::new("node1");
    let weak1 = node1.downgrade();
    let weak2 = weak1.clone();

    let node2 = weak1.upgrade().unwrap();
    assert_eq!(node1, node2);

    let node3 = weak2.upgrade().unwrap();
    assert_eq!(node1, node3);
}

#[test]
fn weak_2() {
    let weak;

    {
        let node1 = Node::new("node1");
        weak = node1.downgrade();
    }

    assert_eq!(None, weak.upgrade());
}

#[test]
fn children_1() {
    let node1 = Node::new("node1");
    let node2 = Node::new("node2");
    let node3 = Node::new("node3");
    node1.append(node2.clone());
    node1.append(node3.clone());

    let mut children = node1.children();

    let c = children.next();
    assert!(c.is_some());
    let c = c.unwrap();
    assert_eq!(c, node2);

    let c = children.next();
    assert!(c.is_some());
    let c = c.unwrap();
    assert_eq!(c, node3);

    assert!(children.next().is_none());
    assert!(children.next_back().is_none());
}

#[test]
fn children_2() {
    let node1 = Node::new("node1");
    let node2 = Node::new("node2");
    let node3 = Node::new("node3");
    node1.append(node2.clone());
    node1.append(node3.clone());

    let mut children = node1.children();

    let c = children.next_back();
    assert!(c.is_some());
    let c = c.unwrap();
    assert_eq!(c, node3);

    let c = children.next_back();
    assert!(c.is_some());
    let c = c.unwrap();
    assert_eq!(c, node2);

    assert!(children.next().is_none());
    assert!(children.next_back().is_none());
}

#[test]
fn traverse_1() {
    let node1 = Node::new("node1");
    let node2 = Node::new("node2");
    let node3 = Node::new("node3");
    node1.append(node2.clone());
    node1.append(node3.clone());

    let mut traverse = node1.traverse();

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node1.clone()));

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node2.clone()));

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node2.clone()));

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node3.clone()));

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node3.clone()));

    let t = traverse.next();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node1.clone()));

    assert!(traverse.next().is_none());
    assert!(traverse.next_back().is_none());
}

#[test]
fn traverse_2() {
    let node1 = Node::new("node1");
    let node2 = Node::new("node2");
    let node3 = Node::new("node3");
    node1.append(node2.clone());
    node1.append(node3.clone());

    let mut traverse = node1.traverse();

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node1.clone()));

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node3.clone()));

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node3.clone()));

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::End(node2.clone()));

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node2.clone()));

    let t = traverse.next_back();
    assert!(t.is_some());
    let t = t.unwrap();
    assert_eq!(t, NodeEdge::Start(node1.clone()));

    assert!(traverse.next().is_none());
    assert!(traverse.next_back().is_none());
}
