//! **This module is experimental**
//!
//! This module provides threadsafe versions of FrozenMap and FrozenVec,
//! ideal for use as a cache.
//!
//! These lock internally, however locks only last as long as the method calls
//!

use stable_deref_trait::StableDeref;
use std::alloc::Layout;
use std::borrow::Borrow;
use std::cmp::Eq;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::iter::{FromIterator, IntoIterator};
use std::ops::Index;

use std::ptr::NonNull;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::RwLock;
use std::sync::TryLockError;

/// Append-only threadsafe version of `std::collections::HashMap` where
/// insertion does not require mutable access
pub struct FrozenMap<K, V> {
    map: RwLock<HashMap<K, V>>,
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for FrozenMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.map.try_read() {
            Ok(guard) => guard.fmt(f),
            Err(TryLockError::Poisoned(err)) => {
                f.debug_tuple("FrozenMap").field(&&**err.get_ref()).finish()
            }
            Err(TryLockError::WouldBlock) => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_tuple("FrozenMap")
                    .field(&LockedPlaceholder)
                    .finish()
            }
        }
    }
}

impl<K, V> Default for FrozenMap<K, V> {
    fn default() -> Self {
        Self {
            map: Default::default(),
        }
    }
}

impl<K, V> FrozenMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> From<Vec<T>> for FrozenVec<T> {
    fn from(vec: Vec<T>) -> Self {
        Self {
            vec: RwLock::new(vec),
        }
    }
}

impl<K: Eq + Hash, V: StableDeref> FrozenMap<K, V> {
    // these should never return &K or &V
    // these should never delete any entries

    /// If the key exists in the map, returns a reference
    /// to the corresponding value, otherwise inserts a
    /// new entry in the map for that key and returns a
    /// reference to the given value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.insert(1, Box::new("a")), &"a");
    /// assert_eq!(map.insert(1, Box::new("b")), &"a");
    /// ```
    pub fn insert(&self, k: K, v: V) -> &V::Target {
        let mut map = self.map.write().unwrap();
        let ret = unsafe {
            let inserted = &**map.entry(k).or_insert(v);
            &*(inserted as *const _)
        };
        ret
    }

    /// If the key exists in the map, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the map for that key and the
    /// value returned by the creation function, and returns a reference to the
    /// generated value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but [`Hash`] and
    /// [`Eq`] on the borrowed form *must* match those for the key type.
    ///
    /// **Note** that the write lock is held for the duration of this function’s
    /// execution, even while the value creation function is executing (if
    /// needed). This will block any concurrent `get` or `insert` calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.insert_with(1, || Box::new("a")), &"a");
    /// assert_eq!(map.insert_with(1, || unreachable!()), &"a");
    /// ```
    pub fn insert_with(&self, k: K, f: impl FnOnce() -> V) -> &V::Target {
        let mut map = self.map.write().unwrap();
        let ret = unsafe {
            let inserted = &**map.entry(k).or_insert_with(f);
            &*(inserted as *const _)
        };
        ret
    }

    /// If the key exists in the map, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the map for that key and the
    /// value returned by the creation function, and returns a reference to the
    /// generated value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but [`Hash`] and
    /// [`Eq`] on the borrowed form *must* match those for the key type.
    ///
    /// **Note** that the write lock is held for the duration of this function’s
    /// execution, even while the value creation function is executing (if
    /// needed). This will block any concurrent `get` or `insert` calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.insert_with_key(1, |_| Box::new("a")), &"a");
    /// assert_eq!(map.insert_with_key(1, |_| unreachable!()), &"a");
    /// ```
    pub fn insert_with_key(&self, k: K, f: impl FnOnce(&K) -> V) -> &V::Target {
        let mut map = self.map.write().unwrap();
        let ret = unsafe {
            let inserted = &**map.entry(k).or_insert_with_key(f);
            &*(inserted as *const _)
        };
        ret
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.get(&1), Some(&"a"));
    /// assert_eq!(map.get(&2), None);
    /// ```
    pub fn get<Q>(&self, k: &Q) -> Option<&V::Target>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let map = self.map.read().unwrap();
        let ret = unsafe { map.get(k).map(|x| &*(&**x as *const V::Target)) };
        ret
    }

    /// Applies a function to the owner of the value corresponding to the key (if any).
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.map_get(&1, Clone::clone), Some(Box::new("a")));
    /// assert_eq!(map.map_get(&2, Clone::clone), None);
    /// ```
    pub fn map_get<Q, T, F>(&self, k: &Q, f: F) -> Option<T>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        F: FnOnce(&V) -> T,
    {
        let map = self.map.read().unwrap();
        let ret = map.get(k).map(f);
        ret
    }
}

impl<K, V> FrozenMap<K, V> {
    /// Collects the contents of this map into a vector of tuples.
    ///
    /// The order of the entries is as if iterating a [`HashMap`] (stochastic).
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// map.insert(1, Box::new("a"));
    /// map.insert(2, Box::new("b"));
    /// let mut tuple_vec = map.into_tuple_vec();
    /// tuple_vec.sort();
    ///
    /// assert_eq!(tuple_vec, vec![(1, Box::new("a")), (2, Box::new("b"))]);
    /// ```
    pub fn into_tuple_vec(self) -> Vec<(K, V)> {
        self.map
            .into_inner()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>()
    }

    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.len(), 0);
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        let map = self.map.read().unwrap();
        map.len()
    }

    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.is_empty(), true);
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.is_empty(), false);
    /// ```
    pub fn is_empty(&self) -> bool {
        let map = self.map.read().unwrap();
        map.is_empty()
    }

    // TODO add more
}

impl<K: Clone, V> FrozenMap<K, V> {
    pub fn keys_cloned(&self) -> Vec<K> {
        self.map.read().unwrap().keys().cloned().collect()
    }
}

impl<K: Eq + Hash, V: Copy> FrozenMap<K, V> {
    /// Returns a copy of the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// map.get_copy_or_insert(1, 6);
    /// assert_eq!(map.get_copy(&1), Some(6));
    /// assert_eq!(map.get_copy(&2), None);
    /// ```
    pub fn get_copy<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let map = self.map.read().unwrap();
        map.get(k).cloned()
    }

    /// If the key exists in the map, returns a reference
    /// to the corresponding value, otherwise inserts a
    /// new entry in the map for that key and returns a
    /// reference to the given value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.get_copy_or_insert(1, 6), 6);
    /// assert_eq!(map.get_copy_or_insert(1, 12), 6);
    /// ```
    pub fn get_copy_or_insert(&self, k: K, v: V) -> V {
        let mut map = self.map.write().unwrap();
        // This is safe because `or_insert` does not overwrite existing values
        let inserted = map.entry(k).or_insert(v);
        *inserted
    }

    /// If the key exists in the map, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the map for that key and the
    /// value returned by the creation function, and returns a reference to the
    /// generated value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but [`Hash`] and
    /// [`Eq`] on the borrowed form *must* match those for the key type.
    ///
    /// **Note** that the write lock is held for the duration of this function’s
    /// execution, even while the value creation function is executing (if
    /// needed). This will block any concurrent `get` or `insert` calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.get_copy_or_insert_with(1, || 6), 6);
    /// assert_eq!(map.get_copy_or_insert_with(1, || unreachable!()), 6);
    /// ```
    pub fn get_copy_or_insert_with(&self, k: K, f: impl FnOnce() -> V) -> V {
        let mut map = self.map.write().unwrap();
        // This is safe because `or_insert_with` does not overwrite existing values
        let inserted = map.entry(k).or_insert_with(f);
        *inserted
    }

    /// If the key exists in the map, returns a reference to the corresponding
    /// value, otherwise inserts a new entry in the map for that key and the
    /// value returned by the creation function, and returns a reference to the
    /// generated value.
    ///
    /// Existing values are never overwritten.
    ///
    /// The key may be any borrowed form of the map's key type, but [`Hash`] and
    /// [`Eq`] on the borrowed form *must* match those for the key type.
    ///
    /// **Note** that the write lock is held for the duration of this function’s
    /// execution, even while the value creation function is executing (if
    /// needed). This will block any concurrent `get` or `insert` calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenMap;
    ///
    /// let map = FrozenMap::new();
    /// assert_eq!(map.get_copy_or_insert_with_key(1, |_| 6), 6);
    /// assert_eq!(map.get_copy_or_insert_with_key(1, |_| unreachable!()), 6);
    /// ```
    pub fn get_copy_or_insert_with_key(&self, k: K, f: impl FnOnce(&K) -> V) -> V {
        let mut map = self.map.write().unwrap();
        // This is safe because `or_insert_with_key` does not overwrite existing values
        let inserted = map.entry(k).or_insert_with_key(f);
        *inserted
    }
}

impl<K, V> std::convert::AsMut<HashMap<K, V>> for FrozenMap<K, V> {
    /// Get mutable access to the underlying [`HashMap`].
    ///
    /// This is safe, as it requires a `&mut self`, ensuring nothing is using
    /// the 'frozen' contents.
    fn as_mut(&mut self) -> &mut HashMap<K, V> {
        self.map.get_mut().unwrap()
    }
}

impl<K: Clone, V: Clone> Clone for FrozenMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.read().unwrap().clone().into(),
        }
    }
}

impl<K: Eq + Hash, V: PartialEq> PartialEq for FrozenMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        let self_ref: &HashMap<K, V> = &self.map.read().unwrap();
        let other_ref: &HashMap<K, V> = &other.map.read().unwrap();
        self_ref == other_ref
    }
}

/// Append-only threadsafe version of `std::vec::Vec` where
/// insertion does not require mutable access
pub struct FrozenVec<T> {
    vec: RwLock<Vec<T>>,
}

impl<T: fmt::Debug> fmt::Debug for FrozenVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.vec.try_read() {
            Ok(guard) => guard.fmt(f),
            Err(TryLockError::Poisoned(err)) => {
                f.debug_tuple("FrozenMap").field(&&**err.get_ref()).finish()
            }
            Err(TryLockError::WouldBlock) => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_tuple("FrozenMap")
                    .field(&LockedPlaceholder)
                    .finish()
            }
        }
    }
}

impl<T> FrozenVec<T> {
    /// Returns the number of elements in the vector.
    pub fn len(&self) -> usize {
        let vec = self.vec.read().unwrap();
        vec.len()
    }

    /// Returns `true` if the vector contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: StableDeref> FrozenVec<T> {
    pub const fn new() -> Self {
        Self {
            vec: RwLock::new(Vec::new()),
        }
    }

    // these should never return &T
    // these should never delete any entries

    pub fn push(&self, val: T) {
        let mut vec = self.vec.write().unwrap();
        vec.push(val);
    }

    /// Push, immediately getting a reference to the element
    pub fn push_get(&self, val: T) -> &T::Target {
        let mut vec = self.vec.write().unwrap();
        vec.push(val);
        unsafe { &*(&**vec.get_unchecked(vec.len() - 1) as *const T::Target) }
    }

    /// Push, immediately getting a an index of the element
    ///
    /// Index can then be used with the `get` method
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenVec;
    ///
    /// let map = FrozenVec::new();
    /// let idx = map.push_get_index(String::from("a"));
    /// assert_eq!(map.get(idx), Some("a"));
    /// assert_eq!(idx, 0);
    /// assert_eq!(map.push_get_index(String::from("b")), 1);
    /// ```
    pub fn push_get_index(&self, val: T) -> usize {
        let mut vec = self.vec.write().unwrap();
        let index = vec.len();
        vec.push(val);
        index
    }

    pub fn get(&self, index: usize) -> Option<&T::Target> {
        let vec = self.vec.read().unwrap();
        unsafe { vec.get(index).map(|x| &*(&**x as *const T::Target)) }
    }

    /// Returns an iterator over the vector.
    pub fn iter(&self) -> Iter<'_, T> {
        self.into_iter()
    }
}

/// Iterator over FrozenVec, obtained via `.iter()`
///
/// It is safe to push to the vector during iteration
#[derive(Debug)]
pub struct Iter<'a, T> {
    vec: &'a FrozenVec<T>,
    idx: usize,
}

impl<'a, T: StableDeref> Iterator for Iter<'a, T> {
    type Item = &'a T::Target;
    fn next(&mut self) -> Option<&'a T::Target> {
        if let Some(ret) = self.vec.get(self.idx) {
            self.idx += 1;
            Some(ret)
        } else {
            None
        }
    }
}

impl<'a, T: StableDeref> IntoIterator for &'a FrozenVec<T> {
    type Item = &'a T::Target;
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Iter<'a, T> {
        Iter { vec: self, idx: 0 }
    }
}

#[test]
fn test_iteration() {
    let vec = vec!["a", "b", "c", "d"];
    let frozen: FrozenVec<_> = vec.clone().into();

    assert_eq!(vec, frozen.iter().collect::<Vec<_>>());
    for (e1, e2) in vec.iter().zip(frozen.iter()) {
        assert_eq!(*e1, e2);
    }

    assert_eq!(vec.len(), frozen.iter().count())
}

impl<T> FrozenVec<T> {
    /// Returns the internal vector backing this structure
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenVec;
    ///
    /// let map = FrozenVec::new();
    /// map.push("a");
    /// map.push("b");
    /// let tuple_vec = map.into_vec();
    ///
    /// assert_eq!(tuple_vec, vec!["a", "b"]);
    /// ```
    pub fn into_vec(self) -> Vec<T> {
        self.vec.into_inner().unwrap()
    }

    // TODO add more
}

impl<T> std::convert::AsMut<Vec<T>> for FrozenVec<T> {
    /// Get mutable access to the underlying vector.
    ///
    /// This is safe, as it requires a `&mut self`, ensuring nothing is using
    /// the 'frozen' contents.
    fn as_mut(&mut self) -> &mut Vec<T> {
        self.vec.get_mut().unwrap()
    }
}

impl<T> Default for FrozenVec<T> {
    fn default() -> Self {
        Self {
            vec: Default::default(),
        }
    }
}

impl<T: Clone> Clone for FrozenVec<T> {
    fn clone(&self) -> Self {
        Self {
            vec: self.vec.read().unwrap().clone().into(),
        }
    }
}

impl<T: PartialEq> PartialEq for FrozenVec<T> {
    fn eq(&self, other: &Self) -> bool {
        let self_ref: &Vec<T> = &self.vec.read().unwrap();
        let other_ref: &Vec<T> = &other.vec.read().unwrap();
        self_ref == other_ref
    }
}

// The context for these functions is that we want to have a
// series of exponentially increasing buffer sizes. We want
// to maximize the total size of the buffers (since this
// determines the maximum size of the container) whilst
// minimizing the number of buffers (since we pay an up-front
// cost in space proportional to the number of buffers)
// without increasing the buffer size too much each time as
// this determines how much space will be wasted on average
// in allocated buffers. Finally, we also want a sequence
// which will generate nice round numbers and is easy to
// work with.

/// we multiply the buffer size by 4 each time whilst sizing
/// the first buffer to 3, so the buffer sizes generated by
/// the function will be 3, 12, 48, 192, etc.
const fn buffer_size(idx: usize) -> usize {
    3 << (idx * 2)
}

/// This computes the sum of the sizes of buffers prior to a
/// particular buffer index, aka `4^idx - 1`. The choice of
/// sequence means that the total buffer size will always be
/// a sequence of `1`s in binary, since it's a power of 2 minus one.
const fn prior_total_buffer_size(idx: usize) -> usize {
    (1 << (idx * 2)) - 1
}

/// This determines which buffer contains the nth item
/// (assuming the items are arranged sequentially in the buffers).
/// Since the total buffer sizes are always sequences of 1s in binary,
/// we can just count the number of binary digits in `(i+1)` and
/// divide by `2` (rounding up).
/// (That's what the `(65 - (i + 1).leading_zeros()) >> 1` part does.)
/// We use 65 rather than `64` so that the division by `2` rounds
/// up instead of down. We divide by `2 (>> 1)` because we skip
/// every other power of `2` since we increase the buffer size by `4`
/// each time, and finally we subtract one because buffer indices are
/// zero-indexed.
const fn buffer_index(i: usize) -> usize {
    (((usize::BITS + 1 - (i + 1).leading_zeros()) >> 1) - 1) as usize
}

/// Each buffer covers 2 bits of address space, so we need half as many
/// buffers as bits in a usize.
const NUM_BUFFERS: usize = (usize::BITS / 2) as usize;

/// Append-only threadsafe version of `std::vec::Vec` where
/// insertion does not require mutable access.
/// Does not lock for reading, only allows `Copy` types and
/// will spinlock on pushes without affecting reads.
/// Note that this data structure is `34` pointers large on
/// 64 bit systems,
/// in contrast to `Vec` which is `3` pointers large.
pub struct LockFreeFrozenVec<T: Copy> {
    data: [AtomicPtr<T>; NUM_BUFFERS],
    len: AtomicUsize,
    locked: AtomicBool,
}

impl<T: Copy> Drop for LockFreeFrozenVec<T> {
    fn drop(&mut self) {
        // We need to drop the elements from all allocated buffers.
        for i in 0..NUM_BUFFERS {
            let layout = Self::layout(buffer_size(i));
            unsafe {
                let ptr = *self.data[i].get_mut();
                if ptr.is_null() {
                    // After the first null pointer there will only be more
                    // null pointers.
                    break;
                } else {
                    std::alloc::dealloc(ptr.cast(), layout);
                }
            }
        }
    }
}

impl<T: Copy> Default for LockFreeFrozenVec<T> {
    /// Creates an empty `LockFreeFrozenVec` that does not allocate
    /// any heap allocations until the first time data is pushed to it.
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> LockFreeFrozenVec<T> {
    const NULL_ATOMIC_PTR: AtomicPtr<T> = AtomicPtr::new(std::ptr::null_mut());

    const fn null() -> [AtomicPtr<T>; NUM_BUFFERS] {
        [Self::NULL_ATOMIC_PTR; NUM_BUFFERS]
    }

    pub const fn new() -> Self {
        Self {
            data: Self::null(),
            len: AtomicUsize::new(0),
            locked: AtomicBool::new(false),
        }
    }

    /// Obtains a write lock that ensures other writing threads
    /// wait for us to finish. Reading threads are unaffected and
    /// can concurrently read while we push a new element.
    fn lock<U>(&self, f: impl FnOnce() -> U) -> U {
        while self.locked.swap(true, Ordering::Acquire) {
            // Wheeeee spinlock
            std::hint::spin_loop();
        }

        let ret = f();
        self.locked.store(false, Ordering::Release);
        ret
    }

    fn layout(cap: usize) -> Layout {
        Layout::array::<T>(cap).unwrap()
    }

    // these should never return &T
    // these should never delete any entries

    const NOT_ZST: () = if std::mem::size_of::<T>() == 0 {
        panic!("`LockFreeFrozenVec` cannot be used with ZSTs");
    };

    /// Pushes an element to the vector, potentially allocating new memory.
    /// Returns the index at which the element was inserted.
    pub fn push(&self, val: T) -> usize {
        // This statement actually does something: it evaluates a constant.
        #[allow(path_statements)]
        {
            Self::NOT_ZST
        }
        self.lock(|| {
            // These values must be consistent with the pointer we got.
            let len = self.len();
            let buffer_idx = buffer_index(len);
            let mut ptr = self.data[buffer_idx].load(Ordering::Acquire);
            if ptr.is_null() {
                // Out of memory, allocate more
                let layout = Self::layout(buffer_size(buffer_idx));
                // SAFETY: `LockFreeFrozenVec` statically rejects zsts and the input `ptr` has always been
                // allocated at the size stated in `cap`.
                unsafe {
                    ptr = std::alloc::alloc(layout).cast::<T>();
                }

                assert!(!ptr.is_null());

                self.data[buffer_idx].store(ptr, Ordering::Release);
            }
            let local_index = len - prior_total_buffer_size(buffer_idx);
            unsafe {
                ptr.add(local_index).write(val);
            }
            // This is written before updating the data pointer. Other `push` calls cannot observe this,
            // because they are blocked on aquiring the data pointer before they ever read the `len`.
            // `get` may read the length without synchronization, but that is fine,
            // as there will be actually the right number of elements stored, or less elements,
            // in which case you get a spurious `None`.
            self.len.store(len + 1, Ordering::Release);
            len
        })
    }

    /// Load an element (if it exists). This operation is lock-free and
    /// performs minimal synchronization.
    pub fn get(&self, index: usize) -> Option<T> {
        // The length can only grow, so just doing the length check
        // independently of the  read is fine. Worst case we
        // read an old length value and end up returning `None` even if
        // another thread already inserted the value.
        if index >= self.len() {
            return None;
        }
        let buffer_idx = buffer_index(index);
        let buffer_ptr = self.data[buffer_idx].load(Ordering::Acquire);
        let local_index = index - prior_total_buffer_size(buffer_idx);
        Some(unsafe { *buffer_ptr.add(local_index) })
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    /// Load an element (if it exists). This operation is lock-free and
    /// performs no synchronized atomic operations. This is a useful primitive to
    /// implement your own data structure with newtypes around the index.
    ///
    /// ## Safety
    ///
    /// `index` must be in bounds, i.e. it must be less than `self.len()`
    #[inline]
    pub unsafe fn get_unchecked(&self, index: usize) -> T {
        let buffer_idx = buffer_index(index);
        let buffer_ptr = self.data[buffer_idx].load(Ordering::Relaxed);
        let local_index = index - prior_total_buffer_size(buffer_idx);
        unsafe { *buffer_ptr.add(local_index) }
    }

    /// Run a function on each buffer in the vector.
    ///
    /// ## Arguments
    /// - `func`: a function that takes a slice to the buffer and the buffer index
    ///
    fn for_each_buffer(&self, mut func: impl FnMut(&[T], usize)) {
        // for each buffer, run the function
        for buffer_index in 0..NUM_BUFFERS {
            // get the buffer pointer
            if let Some(buffer_ptr) = NonNull::new(self.data[buffer_index].load(Ordering::Acquire))
            {
                // get the buffer size and index
                let buffer_size = buffer_size(buffer_index);

                // create a slice from the buffer pointer and size
                let buffer_slice =
                    unsafe { std::slice::from_raw_parts(buffer_ptr.as_ptr(), buffer_size) };

                // run the function
                func(buffer_slice, buffer_index);
            } else {
                // no data in this buffer, so we're done
                break;
            }
        }
    }
}

impl<T: Copy + PartialEq> PartialEq for LockFreeFrozenVec<T> {
    fn eq(&self, other: &Self) -> bool {
        // first check the length
        let self_len = self.len();
        let other_len = other.len();
        if self_len != other_len {
            return false;
        }

        // Since the lengths are the same, just check the elements in order
        for index in 0..self_len {
            // This is safe because the indices are in bounds (for `LockFreeFrozenVec` the bounds can only grow).
            if self.get(index) != other.get(index) {
                return false;
            }
        }

        true
    }
}

#[test]
fn test_non_lockfree_unchecked() {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    struct Moo(i32);

    let vec = LockFreeFrozenVec::new();

    let idx_set = std::sync::Mutex::new(std::collections::HashSet::new());

    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..1000 {
                idx_set.lock().unwrap().insert(vec.push(Moo(i)));
            }
        });
        s.spawn(|| {
            for i in 0..1000 {
                idx_set.lock().unwrap().insert(vec.push(Moo(i)));
            }
        });
        for _ in 0..2000 {
            let idxes = std::mem::take(&mut *idx_set.lock().unwrap());
            for idx in idxes {
                unsafe {
                    vec.get_unchecked(idx);
                }
            }
        }
    });

    // Test dropping empty vecs
    LockFreeFrozenVec::<()>::new();
}

impl<T: Copy + Clone> Clone for LockFreeFrozenVec<T> {
    fn clone(&self) -> Self {
        let mut coppied_data = Self::null();
        // for each buffer, copy the data
        self.for_each_buffer(|buffer_slice, buffer_index| {
            // allocate a new buffer
            let layout = Self::layout(buffer_slice.len());
            let new_buffer_ptr = unsafe { std::alloc::alloc(layout).cast::<T>() };
            assert!(!new_buffer_ptr.is_null());
            // copy the data to the new buffer
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buffer_slice.as_ptr(),
                    new_buffer_ptr,
                    buffer_slice.len(),
                );
            };
            // store the new buffer pointer
            *coppied_data[buffer_index].get_mut() = new_buffer_ptr;
        });

        Self {
            data: coppied_data,
            // Since stores always use `Ordering::Release`, this call to `self.len()` (which uses `Ordering::Acquire`) reults
            // in the operation overall being `Ordering::Relaxed`
            len: AtomicUsize::new(self.len()),
            locked: AtomicBool::new(false),
        }
    }
}

#[test]
fn test_non_lockfree() {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    struct Moo(i32);

    let vec = LockFreeFrozenVec::new();

    assert_eq!(vec.get(1), None);

    vec.push(Moo(1));
    let i = vec.push(Moo(2));
    vec.push(Moo(3));

    assert_eq!(vec.get(i), Some(Moo(2)));

    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..1000 {
                vec.push(Moo(i));
            }
        });
        s.spawn(|| {
            for i in 0..1000 {
                vec.push(Moo(i));
            }
        });
        for i in 0..2000 {
            while vec.get(i).is_none() {}
        }
    });

    // Test cloning
    {
        let vec2 = vec.clone();
        assert_eq!(vec2.get(0), Some(Moo(1)));
        assert_eq!(vec2.get(1), Some(Moo(2)));
        assert_eq!(vec2.get(2), Some(Moo(3)));
    }
    // Test cloning a large vector
    {
        let large_vec = LockFreeFrozenVec::new();
        for i in 0..1000 {
            large_vec.push(Moo(i));
        }
        let large_vec_2 = large_vec.clone();
        for i in 0..1000 {
            assert_eq!(large_vec_2.get(i), Some(Moo(i as i32)));
        }
    }
    // Test cloning an empty vector
    {
        let empty_vec = LockFreeFrozenVec::<()>::new();
        let empty_vec_2 = empty_vec.clone();
        assert_eq!(empty_vec_2.get(0), None);
    }

    // Test dropping empty vecs
    LockFreeFrozenVec::<()>::new();
}

// TODO: Implement IntoIterator for LockFreeFrozenVec

/// Append-only threadsafe version of `std::collections::BTreeMap` where
/// insertion does not require mutable access
#[derive(Debug)]
pub struct FrozenBTreeMap<K, V>(RwLock<BTreeMap<K, V>>);

impl<K: Clone + Ord, V: StableDeref> FrozenBTreeMap<K, V> {
    pub const fn new() -> Self {
        Self(RwLock::new(BTreeMap::new()))
    }

    // these should never return &K or &V
    // these should never delete any entries

    /// Returns a reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Ord`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.get(&1), Some(&"a"));
    /// assert_eq!(map.get(&2), None);
    /// ```
    pub fn get<Q>(&self, k: &Q) -> Option<&V::Target>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let map = self.0.read().unwrap();
        let ret = unsafe { map.get(k).map(|x| &*(&**x as *const V::Target)) };
        ret
    }

    /// Insert a new value into the map. Does nothing if the key is already occupied.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.get(&1), Some(&"a"));
    /// ```
    pub fn insert(&self, k: K, v: V) -> &V::Target {
        let mut map = self.0.write().unwrap();
        let ret = unsafe {
            let inserted = &**map.entry(k).or_insert(v);
            &*(inserted as *const _)
        };
        ret
    }

    /// Applies a function to the owner of the value corresponding to the key (if any).
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Ord`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.map_get(&1, Clone::clone), Some(Box::new("a")));
    /// assert_eq!(map.map_get(&2, Clone::clone), None);
    /// ```
    pub fn map_get<Q, T, F>(&self, k: &Q, f: F) -> Option<T>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        F: FnOnce(&V) -> T,
    {
        let map = self.0.read().unwrap();
        let ret = map.get(k).map(f);
        ret
    }
}

impl<K, V> FrozenBTreeMap<K, V> {
    /// Collects the contents of this map into a vector of tuples.
    ///
    /// The order of the entries is as if iterating a [`BTreeMap`] (ordered by key).
    ///
    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// map.insert(1, Box::new("a"));
    /// map.insert(2, Box::new("b"));
    /// let tuple_vec = map.into_tuple_vec();
    ///
    /// assert_eq!(tuple_vec, vec![(1, Box::new("a")), (2, Box::new("b"))]);
    /// ```
    pub fn into_tuple_vec(self) -> Vec<(K, V)> {
        self.0.into_inner().unwrap().into_iter().collect::<Vec<_>>()
    }

    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// assert_eq!(map.len(), 0);
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        let map = self.0.read().unwrap();
        map.len()
    }

    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// assert_eq!(map.is_empty(), true);
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map.is_empty(), false);
    /// ```
    pub fn is_empty(&self) -> bool {
        let map = self.0.read().unwrap();
        map.is_empty()
    }
}

impl<K: Clone + Ord, V: StableDeref> From<BTreeMap<K, V>> for FrozenBTreeMap<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        Self(RwLock::new(map))
    }
}

impl<Q: ?Sized, K, V> Index<&Q> for FrozenBTreeMap<K, V>
where
    Q: Ord,
    K: Clone + Ord + Borrow<Q>,
    V: StableDeref,
{
    type Output = V::Target;

    /// # Examples
    ///
    /// ```
    /// use elsa::sync::FrozenBTreeMap;
    ///
    /// let map = FrozenBTreeMap::new();
    /// map.insert(1, Box::new("a"));
    /// assert_eq!(map[&1], "a");
    /// ```
    fn index(&self, idx: &Q) -> &V::Target {
        self.get(idx)
            .expect("attempted to index FrozenBTreeMap with unknown key")
    }
}

impl<K: Clone + Ord, V: StableDeref> FromIterator<(K, V)> for FrozenBTreeMap<K, V> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (K, V)>,
    {
        let map: BTreeMap<_, _> = iter.into_iter().collect();
        map.into()
    }
}

impl<K: Clone + Ord, V: StableDeref> Default for FrozenBTreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone, V: Clone> Clone for FrozenBTreeMap<K, V> {
    fn clone(&self) -> Self {
        Self(self.0.read().unwrap().clone().into())
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for FrozenBTreeMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        let self_ref: &BTreeMap<K, V> = &self.0.read().unwrap();
        let other_ref: &BTreeMap<K, V> = &other.0.read().unwrap();
        self_ref == other_ref
    }
}
