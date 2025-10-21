use std::cell::{Cell, UnsafeCell};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::iter::FromIterator;
use std::ops::Index;

use indexmap::IndexMap;
use stable_deref_trait::StableDeref;

pub use indexmap::Equivalent;

/// Append-only version of `indexmap::IndexMap` where
/// insertion does not require mutable access
pub struct FrozenIndexMap<K, V, S = RandomState> {
    map: UnsafeCell<IndexMap<K, V, S>>,
    /// Eq/Hash implementations can have side-effects, and using Rc it is possible
    /// for FrozenIndexMap::insert to be called on a key that itself contains the same
    /// `FrozenIndexMap`, whose `eq` implementation also calls FrozenIndexMap::insert
    ///
    /// We use this `in_use` flag to guard against any reentrancy.
    in_use: Cell<bool>,
}

// safety: UnsafeCell implies !Sync

impl<K: Eq + Hash, V> FrozenIndexMap<K, V> {
    pub fn new() -> Self {
        Self {
            map: UnsafeCell::new(Default::default()),
            in_use: Cell::new(false),
        }
    }
}

impl<K: Eq + Hash, V: StableDeref, S: BuildHasher> FrozenIndexMap<K, V, S> {
    // these should never return &K or &V
    // these should never delete any entries
    //
    /// If the key exists in the map, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the map for that key
    /// and returns a reference to the generated value.
    ///
    /// Existing values are never overwritten.
    ///
    /// # Example
    /// ```
    /// use elsa::index_map::FrozenIndexMap;
    /// let map = FrozenIndexMap::new();
    /// assert_eq!(map.insert(1, Box::new("a")), &"a");
    /// assert_eq!(map.insert(1, Box::new("b")), &"a");
    /// ```
    pub fn insert(&self, k: K, v: V) -> &V::Target {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            &*(*map).entry(k).or_insert(v)
        };
        self.in_use.set(false);
        ret
    }

    // these should never return &K or &V
    // these should never delete any entries
    //
    /// If the key exists in the map, returns a reference to the corresponding
    /// value and its index, otherwise inserts a new entry in the map for that key
    /// and returns a reference to the generated value and its index.
    ///
    /// Existing values are never overwritten.
    ///
    /// # Example
    /// ```
    /// use elsa::index_map::FrozenIndexMap;
    /// let map = FrozenIndexMap::new();
    /// assert_eq!(map.insert_full(12, Box::new("a")), (0, &"a"));
    /// assert_eq!(map.insert_full(12, Box::new("b")), (0, &"a"));
    /// ```
    pub fn insert_full(&self, k: K, v: V) -> (usize, &V::Target) {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            let entry = (*map).entry(k);
            let index = entry.index();
            (index, &**entry.or_insert(v))
        };
        self.in_use.set(false);
        ret
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<K>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.get(&1), Some(&"a"));
    /// assert_eq!(map.get(&2), None);
    /// ```
    pub fn get<Q>(&self, k: &Q) -> Option<&V::Target>
    where
        Q: ?Sized + Hash + Equivalent<K>,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).get(k).map(|x| &**x)
        };
        self.in_use.set(false);
        ret
    }

    /// Returns the index corresponding to the key
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<K>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_map::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.get_index_of(&1), Some(0));
    /// assert_eq!(map.get_index_of(&2), None);
    /// ```
    pub fn get_index_of<Q>(&self, k: &Q) -> Option<usize>
    where
        Q: ?Sized + Hash + Equivalent<K>,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).get_index_of(k)
        };
        self.in_use.set(false);
        ret
    }

    /// Returns a reference to the key-value mapping corresponding to an index.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_map::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// let (idx, _ref) = map.insert_full(Box::new("foo"), Box::new("a"));
    /// assert_eq!(idx, 0);
    /// assert_eq!(map.get_index(idx), Some((&"foo", &"a")));
    /// assert_eq!(map.get_index(idx + 1), None);
    /// ```
    pub fn get_index(&self, index: usize) -> Option<(&K::Target, &V::Target)>
    where
        K: StableDeref,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).get_index(index).map(|(k, v)| (&**k, &**v))
        };
        self.in_use.set(false);
        ret
    }

    /// Returns a reference to the key, along with its index and a reference to its value
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<K>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::index_map::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert("1", Box::new("a"));
    /// assert_eq!(map.get_full("1"), Some((0, "1", &"a")));
    /// assert_eq!(map.get_full("2"), None);
    /// ```
    pub fn get_full<Q>(&self, k: &Q) -> Option<(usize, &K::Target, &V::Target)>
    where
        Q: ?Sized + Hash + Equivalent<K>,
        K: StableDeref,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).get_full(k).map(|(i, k, v)| (i, &**k, &**v))
        };
        self.in_use.set(false);
        ret
    }

    /// Applies a function to the owner of the value corresponding to the key (if any).
    ///
    /// # Arguments
    /// * `k` may be any type that implements [`Equivalent<K>`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.map_get(&1, Clone::clone), Some(Box::new("a")));
    /// assert_eq!(map.map_get(&2, Clone::clone), None);
    /// ```
    pub fn map_get<Q, T, F>(&self, k: &Q, f: F) -> Option<T>
    where
        Q: ?Sized + Hash + Equivalent<K>,
        F: FnOnce(&V) -> T,
    {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).get(k).map(f)
        };
        self.in_use.set(false);
        ret
    }
}

impl<K, V, S> FrozenIndexMap<K, V, S> {
    /// Collects the contents of this map into a vector of tuples.
    ///
    /// The order of the entries is as if iterating an [`IndexMap`].
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert(1, Box::new("a"));
    /// map.insert(2, Box::new("b"));
    /// let tuple_vec = map.into_tuple_vec();
    ///
    /// assert_eq!(tuple_vec, vec![(1, Box::new("a")), (2, Box::new("b"))]);
    /// ```
    pub fn into_tuple_vec(self) -> Vec<(K, V)> {
        self.map.into_inner().into_iter().collect::<Vec<_>>()
    }

    pub fn into_map(self) -> IndexMap<K, V, S> {
        self.map.into_inner()
    }

    /// Get mutable access to the underlying [`IndexMap`].
    ///
    /// This is safe, as it requires a `&mut self`, ensuring nothing is using
    /// the 'frozen' contents.
    pub fn as_mut(&mut self) -> &mut IndexMap<K, V, S> {
        unsafe { &mut *self.map.get() }
    }

    /// Returns true if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let ret = unsafe {
            let map = self.map.get();
            (*map).is_empty()
        };
        self.in_use.set(false);
        ret
    }
}

impl<K, V, S> From<IndexMap<K, V, S>> for FrozenIndexMap<K, V, S> {
    fn from(map: IndexMap<K, V, S>) -> Self {
        Self {
            map: UnsafeCell::new(map),
            in_use: Cell::new(false),
        }
    }
}

impl<Q, K, V, S> Index<&Q> for FrozenIndexMap<K, V, S>
where
    Q: ?Sized + Hash + Equivalent<K>,
    K: Eq + Hash,
    V: StableDeref,
    S: BuildHasher,
{
    type Output = V::Target;

    /// # Examples
    ///
    /// ```
    /// use elsa::FrozenIndexMap;
    ///
    /// let map = FrozenIndexMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map[&1], "a");
    /// ```
    fn index(&self, idx: &Q) -> &V::Target {
        self.get(idx)
            .expect("attempted to index FrozenIndexMap with unknown key")
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> FromIterator<(K, V)> for FrozenIndexMap<K, V, S> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (K, V)>,
    {
        let map: IndexMap<_, _, _> = iter.into_iter().collect();
        map.into()
    }
}

impl<K: Eq + Hash, V, S: Default> Default for FrozenIndexMap<K, V, S> {
    fn default() -> Self {
        Self {
            map: UnsafeCell::new(Default::default()),
            in_use: Cell::new(false),
        }
    }
}

impl<K: Clone, V: Clone, S: Clone> Clone for FrozenIndexMap<K, V, S> {
    fn clone(&self) -> Self {
        assert!(!self.in_use.get());
        self.in_use.set(true);
        let self_clone = Self {
            map: unsafe { self.map.get().as_ref().unwrap() }.clone().into(),
            in_use: Cell::from(false),
        };
        self.in_use.set(false);
        return self_clone;
    }
}

impl<T: Hash + Eq, S: PartialEq> PartialEq for FrozenIndexMap<T, S> {
    fn eq(&self, other: &Self) -> bool {
        assert!(!self.in_use.get());
        assert!(!other.in_use.get());
        self.in_use.set(true);
        other.in_use.set(true);
        let ret = unsafe { self.map.get().as_ref() == other.map.get().as_ref() };
        self.in_use.set(false);
        other.in_use.set(false);
        ret
    }
}
