use std::{
    collections::{BTreeSet, HashSet},
    hash::Hash,
};

use crate::timeline::MergeKind;

/// Affords:
/// - O(1) contains
/// - O(log n) sorted insertion
pub struct HybridSet<T> {
    reversed: bool,
    lookup: HashSet<T>,   // fast deduplication
    ordered: BTreeSet<T>, // sorted iteration
}

impl<T> Default for HybridSet<T> {
    fn default() -> Self {
        Self {
            reversed: Default::default(),
            lookup: Default::default(),
            ordered: Default::default(),
        }
    }
}

pub enum InsertionResponse {
    AlreadyExists,
    Merged(MergeKind),
}

impl<T: Copy + Ord + Eq + Hash> HybridSet<T> {
    pub fn insert(&mut self, val: T) -> InsertionResponse {
        if !self.lookup.insert(val) {
            return InsertionResponse::AlreadyExists;
        }

        let front_insertion = match self.ordered.iter().next() {
            Some(first) => (val >= *first) == self.reversed,
            None => true,
        };

        self.ordered.insert(val); // O(log n)

        InsertionResponse::Merged(if front_insertion {
            MergeKind::FrontInsert
        } else {
            MergeKind::Spliced
        })
    }
}

impl<T: Eq + Hash> HybridSet<T> {
    pub fn contains(&self, val: &T) -> bool {
        self.lookup.contains(val) // O(1)
    }
}

impl<T> HybridSet<T> {
    pub fn iter(&self) -> HybridIter<'_, T> {
        HybridIter {
            inner: self.ordered.iter(),
            reversed: self.reversed,
        }
    }

    pub fn new(reversed: bool) -> Self {
        Self {
            reversed,
            ..Default::default()
        }
    }
}

impl<'a, T> IntoIterator for &'a HybridSet<T> {
    type Item = &'a T;
    type IntoIter = HybridIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct HybridIter<'a, T> {
    inner: std::collections::btree_set::Iter<'a, T>,
    reversed: bool,
}

impl<'a, T> Iterator for HybridIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.reversed {
            self.inner.next_back()
        } else {
            self.inner.next()
        }
    }
}
