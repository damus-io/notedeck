// Copyright (c) 2024 Yuki Kishimoto
// Distributed under the MIT software license

//! Atomic destructor

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::sync::Arc;
use core::fmt::{self, Debug};
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

trait SaturatingUsize {
    fn saturating_increment(&self, order: Ordering) -> usize;

    fn saturating_decrement(&self, order: Ordering) -> usize;
}

impl SaturatingUsize for AtomicUsize {
    /// Atomically increments the AtomicUsize by 1, saturating at `usize::MAX`.
    ///
    /// Return the new value or `usize::MAX`.
    fn saturating_increment(&self, order: Ordering) -> usize {
        loop {
            let current: usize = self.load(order);

            if current == usize::MAX {
                // Already at maximum, cannot increment further
                return current;
            }

            let new: usize = current + 1;
            match self.compare_exchange(current, new, order, order) {
                Ok(_) => return new,
                Err(_) => continue, // Retry if the value changed concurrently
            }
        }
    }

    /// Atomically decrements the AtomicUsize by 1, saturating at `0`.
    ///
    /// Return the new value or `0`.
    fn saturating_decrement(&self, order: Ordering) -> usize {
        loop {
            let current: usize = self.load(order);

            if current == 0 {
                // Already at minimum, cannot decrement further
                return current;
            }

            let new: usize = current - 1;
            match self.compare_exchange(current, new, order, order) {
                Ok(_) => return new,
                Err(_) => continue, // Retry if the value changed concurrently
            }
        }
    }
}

/// Stealth clone
pub trait StealthClone {
    /// Clone without increment the atomic destructor counter.
    ///
    /// Items that are stealth cloned, NOT decrement the counter when dropped.
    fn stealth_clone(&self) -> Self;
}

/// Atomic destroyer
pub trait AtomicDestroyer: Debug + Clone {
    /// Instructions to execute when all instances are dropped
    fn on_destroy(&self);
}

/// Atomic destructor
pub struct AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    destroyed: Arc<AtomicBool>,
    counter: Arc<AtomicUsize>,
    stealth: bool,
    inner: T,
}

impl<T> Deref for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> Debug for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicDestructor")
            .field("destroyed", &self.destroyed)
            .field("counter", &self.counter)
            .field("stealth", &self.stealth)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T> Clone for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    fn clone(&self) -> Self {
        // Increase counter
        self.counter.saturating_increment(Ordering::SeqCst);

        // Clone
        Self {
            destroyed: self.destroyed.clone(),
            counter: self.counter.clone(),
            stealth: false,
            inner: self.inner.clone(),
        }
    }
}

impl<T> StealthClone for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    fn stealth_clone(&self) -> Self {
        Self {
            destroyed: self.destroyed.clone(),
            counter: self.counter.clone(),
            stealth: true,
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    fn drop(&mut self) {
        // Stealth or already destroyed, immediately return
        if self.is_stealth() || self.is_destroyed() {
            return;
        }

        // Decrease counter
        let value: usize = self.counter.saturating_decrement(Ordering::SeqCst);

        // Check if it's time for destruction
        if value == 0 {
            // Destroy
            self.inner.on_destroy();

            // Mark as destroyed
            self.destroyed.store(true, Ordering::SeqCst);
        }
    }
}

impl<T> AtomicDestructor<T>
where
    T: AtomicDestroyer,
{
    /// New wrapper
    pub fn new(inner: T) -> Self {
        Self {
            destroyed: Arc::new(AtomicBool::new(false)),
            counter: Arc::new(AtomicUsize::new(1)),
            stealth: false,
            inner,
        }
    }

    /// Get counter
    pub fn counter(&self) -> usize {
        self.counter.load(Ordering::SeqCst)
    }

    /// Check if destroyed
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    /// Check if is stealth (stealth cloned, not subject to counter increase/decrease)
    pub fn is_stealth(&self) -> bool {
        self.stealth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct InternalTestingStealth;

    impl AtomicDestroyer for InternalTestingStealth {
        fn on_destroy(&self) {}
    }

    #[derive(Clone)]
    struct TestingStealth {
        inner: AtomicDestructor<InternalTestingStealth>,
    }

    impl StealthClone for TestingStealth {
        fn stealth_clone(&self) -> Self {
            Self {
                inner: self.inner.stealth_clone(),
            }
        }
    }

    impl TestingStealth {
        pub fn new() -> Self {
            Self {
                inner: AtomicDestructor::new(InternalTestingStealth),
            }
        }
    }

    #[test]
    fn test_clone() {
        let t = TestingStealth::new();
        assert_eq!(t.inner.counter(), 1);
        assert!(!t.inner.is_stealth());
        assert!(!t.inner.is_destroyed());

        let t_1 = t.clone();
        assert_eq!(t.inner.counter(), 2);
        assert_eq!(t_1.inner.counter(), 2);

        let t_2 = t_1.clone();
        assert_eq!(t.inner.counter(), 3);
        assert_eq!(t_1.inner.counter(), 3);
        assert_eq!(t_2.inner.counter(), 3);

        drop(t_1);
        assert_eq!(t.inner.counter(), 2);
        assert!(!t.inner.is_destroyed());

        drop(t_2);
        assert_eq!(t.inner.counter(), 1);
    }

    #[test]
    fn test_stealth_clone() {
        let t = TestingStealth::new();
        assert_eq!(t.inner.counter(), 1);
        assert!(!t.inner.is_stealth());

        let t_1 = t.stealth_clone();
        assert_eq!(t.inner.counter(), 1);
        assert_eq!(t_1.inner.counter(), 1);
        assert!(!t.inner.is_stealth());
        assert!(t_1.inner.is_stealth());

        let t_2 = t_1.clone(); // Cloning stealth destructor
        assert_eq!(t.inner.counter(), 2);
        assert_eq!(t_1.inner.counter(), 2);
        assert_eq!(t_2.inner.counter(), 2);

        let t_3 = t.clone(); // Cloning NON-stealth destructor
        assert_eq!(t.inner.counter(), 3);
        assert_eq!(t_1.inner.counter(), 3);
        assert_eq!(t_2.inner.counter(), 3);

        drop(t_1); // Stealth
        assert_eq!(t.inner.counter(), 3);

        drop(t_2); // Classical
        assert_eq!(t.inner.counter(), 2);

        drop(t_3); // Classical
        assert_eq!(t.inner.counter(), 1);
    }
}
