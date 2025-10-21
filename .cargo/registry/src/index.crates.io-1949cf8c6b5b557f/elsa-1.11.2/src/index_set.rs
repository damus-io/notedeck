use std::cell::{Cell, UnsafeCell};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::iter::FromIterator;
use std::ops::Index;

use indexmap::IndexSet;
use stable_deref_trait::StableDeref;

use crate::index_map::Equivalent;

/// Append-only version of `indexmap::IndexSet` where
/// insertion does not require mutable access
pub struct FrozenIndexSet<T, S = RandomState> {
    set: UnsafeCell<IndexSet<T, S>>,
    /// Eq/Hash implementations can have side-effects, and using Rc it is possible
    /// for FrozenIndexSet::insert to be called on a key that itself contains the same
    /// `FrozenIndexSet`, whose `eq` implementation also calls FrozenIndexSet::insert
    ///
    /// We use this `in_use` flag to guard against any reentrancy.
    in_use: Cell<bool>,
}

// safety: UnsafeCell implies !Sync

impl<T: Eq + Hash> FrozenIndexSet<T> {
    pub fn new() -> Self {
        Self::from(IndexSet::new())
    }
}

impl<T: Eq + Hash + StableDeref, S: BuildHasher> FrozenIndexSet<T, S> {
    // these should never return &T
    // these should never delete any entries
    //
    /// If the value exists in the set, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the set for that value
    /// and returns a reference to it.
    ///
    /// Existing values are never overwritten.
    ///
    /// # Example
    /// ```
    /// use elsa::index_set::FrozenIndexSet;
    /// let set = FrozenIndexSet::new();
    /// let a_ref = set.insert(Box::new("a"));
    /// let aa = "a";
    /// let other_a_ref = unsafe { aa.as_ptr() as *const &str};
    /// let other_a = Box::new(aa);
    /// assert!(!std::ptr::eq(a_ref, other_a_ref));
    /// assert!(std::ptr::eq(a_ref, set.insert(other_a)));
    /// ```
    pub fn insert(&self, value: T) -> &T::Target {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            let (index, _was_vacant) = (*set).insert_full(value);
            &*(*set)[index]
        };
        self.in_use.set(false);
        ret
    }

    // these should never return &T
    // these should never delete any entries
    /// If the key exists in the set, returns a reference to the corresponding
    /// value and its index, otherwise inserts a new entry in the set for that value
    /// and returns a reference to it and its index.
    ///
    /// Existing values are never overwritten.
    ///
    /// # Example
    /// ```
    /// use elsa::index_set::FrozenIndexSet;
    /// let set = FrozenIndexSet::new();
    /// assert_eq!(set.insert_full(Box::new("a")), (0, &"a"));
    /// assert_eq!(set.insert_full(Box::new("b")), (1, &"b"));
    /// ```
    pub fn insert_full(&self, value: T) -> (usize, &T::Target) {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            let (index, _was_vacant) = (*set).insert_full(value);
            (index, &*(*set)[index])
        };
        self.in_use.set(false);
        ret
    }

    // TODO implement in case the standard Entry API gets improved
    // // TODO avoid double lookup
    // pub fn entry<Q: ?Sized>(&self, value: &Q) -> Entry<T, Q>
    //     where Q: Hash + Equivalent<T> + ToOwned<Owned = T>
    // {
    //     assert!(!self.in_use.get());
    //     self.in_use.set(true);
    //     unsafe {
    //         let set = self.set.get();
    //         match (*set).get_full(value) {
    //             Some((index, reference)) => {
    //                 Entry::Occupied(OccupiedEntry {
    //                     index,
    //                     reference,
    //                     set: &*set,
    //                 })
    //             }
    //             None => {
    //                 Entry::Vacant(VacantEntry {
    //                     value: Cow::Borrowed(value),
    //                     set: &*set,
    //                 })
    //             }
    //         }
    //     }
    // }

    /// Returns a reference to the value passed as argument if present in the set.
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<T>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_set::FrozenIndexSet;
    ///
    /// let set = FrozenIndexSet::new();
    /// set.insert(Box::new("a"));
    /// assert_eq!(set.get(&Box::new("a")), Some(&"a"));
    /// assert_eq!(set.get(&Box::new("b")), None);
    /// ```
    pub fn get<Q>(&self, k: &Q) -> Option<&T::Target>
    where
        Q: ?Sized + Hash + Equivalent<T>,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            (*set).get(k).map(|x| &**x)
        };
        self.in_use.set(false);
        ret
    }

    /// Returns the index corresponding to the value if present in the set
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<T>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::FrozenIndexSet;
    ///
    /// let set = FrozenIndexSet::new();
    /// set.insert(Box::new("a"));
    /// assert_eq!(set.get_index_of(&Box::new("a")), Some(0));
    /// assert_eq!(set.get_index_of(&Box::new("b")), None);
    /// ```
    pub fn get_index_of<Q>(&self, k: &Q) -> Option<usize>
    where
        Q: ?Sized + Hash + Equivalent<T>,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            (*set).get_index_of(k)
        };
        self.in_use.set(false);
        ret
    }

    /// Returns a reference to the value passed as argument if present in the set,
    /// along with its index
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<T>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_set::FrozenIndexSet;
    ///
    /// let set = FrozenIndexSet::new();
    /// set.insert(Box::new("a"));
    /// assert_eq!(set.get_full(&Box::new("a")), Some((0, &"a")));
    /// assert_eq!(set.get_full(&Box::new("b")), None);
    /// ```
    pub fn get_full<Q>(&self, k: &Q) -> Option<(usize, &T::Target)>
    where
        Q: ?Sized + Hash + Equivalent<T>,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            (*set).get_full(k).map(|(i, x)| (i, &**x))
        };
        self.in_use.set(false);
        ret
    }

    /// Returns a reference to value at the index passed as argument, if
    /// present in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_set::FrozenIndexSet;
    ///
    /// let set = FrozenIndexSet::new();
    /// set.insert(Box::new("a"));
    /// assert_eq!(set.get_index(0), Some(&"a"));
    /// assert_eq!(set.get_index(1), None);
    /// ```
    pub fn get_index(&self, index: usize) -> Option<&T::Target> {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            (*set).get_index(index).map(|r| &**r)
        };
        self.in_use.set(false);
        ret
    }
}

impl<T, S> FrozenIndexSet<T, S> {
    pub fn into_set(self) -> IndexSet<T, S> {
        self.set.into_inner()
    }

    /// Get mutable access to the underlying [`IndexSet`].
    ///
    /// This is safe, as it requires a `&mut self`, ensuring nothing is using
    /// the 'frozen' contents.
    pub fn as_mut(&mut self) -> &mut IndexSet<T, S> {
        unsafe { &mut *self.set.get() }
    }

    // TODO add more
}

impl<T, S> From<IndexSet<T, S>> for FrozenIndexSet<T, S> {
    fn from(set: IndexSet<T, S>) -> Self {
        Self {
            set: UnsafeCell::new(set),
            in_use: Cell::new(false),
        }
    }
}

impl<T: Eq + Hash + StableDeref, S> Index<usize> for FrozenIndexSet<T, S> {
    type Output = T::Target;
    fn index(&self, idx: usize) -> &T::Target {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let set = self.set.get();
            &*(*set)[idx]
        };
        self.in_use.set(false);
        ret
    }
}

impl<T: Eq + Hash, S: Default + BuildHasher> FromIterator<T> for FrozenIndexSet<T, S> {
    fn from_iter<U>(iter: U) -> Self
    where
        U: IntoIterator<Item = T>,
    {
        let set: IndexSet<_, _> = iter.into_iter().collect();
        set.into()
    }
}

impl<T: Eq + Hash, S: Default> Default for FrozenIndexSet<T, S> {
    fn default() -> Self {
        Self::from(IndexSet::default())
    }
}

impl<K: Clone, V: Clone> Clone for FrozenIndexSet<K, V> {
    fn clone(&self) -> Self {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let self_clone = Self {
            set: unsafe { self.set.get().as_ref().unwrap() }.clone().into(),
            in_use: Cell::from(false),
        };
        self.in_use.set(false);
        return self_clone;
    }
}

impl<T: Hash + Eq, S: BuildHasher> PartialEq for FrozenIndexSet<T, S> {
    fn eq(&self, other: &Self) -> bool {
        assert!(!self.in_use.get());
        assert!(!other.in_use.get());
        self.in_use.set(true);
        other.in_use.set(true);
        let ret = unsafe { self.set.get().as_ref() == other.set.get().as_ref() };
        self.in_use.set(false);
        other.in_use.set(false);
        ret
    }
}
