// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

//! Module that contains the various storage implementations

use alloc::vec::Vec;

use crate::types::{Accumulator, Bound, Fingerprint, Item};
use crate::{Error, Id};

/// NegentropyStorageBase
pub trait NegentropyStorageBase {
    /// Size
    fn size(&self) -> Result<usize, Error>;

    /// Get Item
    fn get_item(&self, i: usize) -> Result<Option<Item>, Error>;

    /// Iterate
    fn iterate(
        &self,
        begin: usize,
        end: usize,
        cb: &mut dyn FnMut(Item, usize) -> Result<bool, Error>,
    ) -> Result<(), Error>;

    /// Find Lower Bound
    fn find_lower_bound(&self, first: usize, last: usize, value: &Bound) -> usize;

    /// Fingerprint
    fn fingerprint(&self, begin: usize, end: usize) -> Result<Fingerprint, Error> {
        let mut out = Accumulator::new();

        self.iterate(begin, end, &mut |item: Item, _| {
            out.add(&item.id)?;
            Ok(true)
        })?;

        out.get_fingerprint((end - begin) as u64)
    }
}

/// Negentropy Storage Vector
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NegentropyStorageVector {
    items: Vec<Item>,
    sealed: bool,
}

impl NegentropyStorageBase for NegentropyStorageVector {
    fn size(&self) -> Result<usize, Error> {
        self.check_sealed()?;
        Ok(self.items.len())
    }

    fn get_item(&self, i: usize) -> Result<Option<Item>, Error> {
        self.check_sealed()?;
        Ok(self.items.get(i).copied())
    }

    fn iterate(
        &self,
        begin: usize,
        end: usize,
        cb: &mut dyn FnMut(Item, usize) -> Result<bool, Error>,
    ) -> Result<(), Error> {
        self.check_sealed()?;
        self.check_bounds(begin, end)?;

        for i in begin..end {
            if !cb(self.items[i], i)? {
                break;
            }
        }

        Ok(())
    }

    fn find_lower_bound(&self, mut first: usize, last: usize, value: &Bound) -> usize {
        let mut count: usize = last - first;

        while count > 0 {
            let mut it: usize = first;
            let step: usize = count / 2;
            it += step;

            if self.items[it] < value.item {
                it += 1;
                first = it;
                count -= step + 1;
            } else {
                count = step;
            }
        }

        first
    }
}

impl NegentropyStorageVector {
    /// Create new storage
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create new storage with capacity
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
            sealed: false,
        }
    }

    /// Insert item
    pub fn insert(&mut self, created_at: u64, id: Id) -> Result<(), Error> {
        if self.sealed {
            return Err(Error::AlreadySealed);
        }

        let elem: Item = Item::with_timestamp_and_id(created_at, id);
        self.items.push(elem);

        Ok(())
    }

    /// Seal
    pub fn seal(&mut self) -> Result<(), Error> {
        if self.sealed {
            return Err(Error::AlreadySealed);
        }
        self.sealed = true;

        self.items.sort();
        self.items.dedup();

        Ok(())
    }

    /// Unseal
    pub fn unseal(&mut self) -> Result<(), Error> {
        self.sealed = false;
        Ok(())
    }

    fn check_sealed(&self) -> Result<(), Error> {
        if !self.sealed {
            return Err(Error::NotSealed);
        }
        Ok(())
    }

    fn check_bounds(&self, begin: usize, end: usize) -> Result<(), Error> {
        if begin > end || end > self.items.len() {
            return Err(Error::BadRange);
        }
        Ok(())
    }
}
