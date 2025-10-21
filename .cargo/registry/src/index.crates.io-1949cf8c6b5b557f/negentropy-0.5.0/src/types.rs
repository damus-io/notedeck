// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use alloc::vec::Vec;
use core::cmp::Ordering;
use core::convert::{TryFrom, TryInto};
use core::num::Wrapping;
use core::ops::Deref;

use crate::encoding::encode_var_int;
use crate::{sha256, Error, Id, FINGERPRINT_SIZE, ID_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Mode {
    Skip = 0,
    Fingerprint = 1,
    IdList = 2,
}

impl Mode {
    pub fn as_u64(&self) -> u64 {
        *self as u64
    }
}

impl TryFrom<u64> for Mode {
    type Error = Error;
    fn try_from(mode: u64) -> Result<Self, Self::Error> {
        match mode {
            0 => Ok(Mode::Skip),
            1 => Ok(Mode::Fingerprint),
            2 => Ok(Mode::IdList),
            m => Err(Error::UnexpectedMode(m)),
        }
    }
}

/// Item
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Item {
    /// timestamp
    pub timestamp: u64,
    /// Id
    pub id: Id,
}

impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Item {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.timestamp != other.timestamp {
            self.timestamp.cmp(&other.timestamp)
        } else {
            self.id.cmp(&other.id)
        }
    }
}

impl Item {
    /// new Item
    pub fn new() -> Self {
        Self::default()
    }

    /// new Item with just timestamp, id is 0s
    pub fn with_timestamp(timestamp: u64) -> Self {
        let mut item = Self::new();
        item.timestamp = timestamp;
        item
    }

    /// new Item with timestamp and id
    #[inline]
    pub fn with_timestamp_and_id(timestamp: u64, id: Id) -> Self {
        Self { timestamp, id }
    }

    /// get id
    pub fn get_id(&self) -> &Id {
        &self.id
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Bound
pub struct Bound {
    /// Item
    pub item: Item,
    /// ID Len
    pub id_len: usize,
}

impl Bound {
    /// New Bound
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// new Bound from item
    pub fn from_item(item: &Item) -> Self {
        let mut bound = Self::new();
        bound.item = *item;
        bound.id_len = ID_SIZE;
        bound
    }

    /// new Bound from timestamp, id len is 0
    pub fn with_timestamp(timestamp: u64) -> Self {
        let mut bound = Self::new();
        bound.item.timestamp = timestamp;
        bound.id_len = 0;
        bound
    }

    /// New Bound from timestamp and id
    pub fn with_timestamp_and_id<T>(timestamp: u64, id: T) -> Result<Self, Error>
    where
        T: AsRef<[u8]>,
    {
        let id: &[u8] = id.as_ref();
        let len: usize = id.len();

        if len > ID_SIZE {
            return Err(Error::IdTooBig);
        }

        let mut out = Bound::new();
        out.item.timestamp = timestamp;
        out.item.id[..len].copy_from_slice(id);
        out.id_len = len;

        Ok(out)
    }
}

impl PartialOrd for Bound {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bound {
    fn cmp(&self, other: &Self) -> Ordering {
        self.item.cmp(&other.item)
    }
}

/// Fingerprint
#[derive(Debug, Clone, Copy, Default)]
pub struct Fingerprint {
    /// Buffer
    buf: [u8; FINGERPRINT_SIZE],
}

impl Deref for Fingerprint {
    type Target = [u8; FINGERPRINT_SIZE];
    fn deref(&self) -> &Self::Target {
        &self.buf
    }
}

impl Fingerprint {
    #[inline]
    pub fn to_bytes(self) -> [u8; FINGERPRINT_SIZE] {
        self.buf
    }
}

/// Accumulator
#[derive(Debug, Clone, Copy, Default)]
pub struct Accumulator {
    buf: [u8; ID_SIZE],
}

impl Accumulator {
    /// New Accumulator
    pub fn new() -> Self {
        Self { buf: [0; ID_SIZE] }
    }

    /* /// Add Item
    pub fn add_item(&mut self, item: &Item) {
        self.add(&item.id);
    }

    /// Add Accum
    pub fn add_accum(&mut self, accum: &Accumulator) {
        self.add(&accum.buf);
    } */

    /// Add
    pub fn add(&mut self, buf: &[u8; ID_SIZE]) -> Result<(), Error> {
        let mut curr_carry = Wrapping(0u64);
        let mut next_carry = Wrapping(0u64);

        let p = &self.buf[..];
        let po = buf;

        let mut wtr = Vec::with_capacity(ID_SIZE);

        for i in 0..4 {
            let orig = Wrapping(u64::from_le_bytes(p[(i * 8)..(i * 8 + 8)].try_into()?));
            let other_v = Wrapping(u64::from_le_bytes(po[(i * 8)..(i * 8 + 8)].try_into()?));

            let mut next = orig;

            next += curr_carry;
            if next < orig {
                next_carry = Wrapping(1u64);
            }

            next += other_v;
            if next < other_v {
                next_carry = Wrapping(1u64);
            }

            wtr.extend_from_slice(&next.0.to_le_bytes());
            curr_carry = next_carry;
            next_carry = Wrapping(0u64);
        }

        self.buf.copy_from_slice(&wtr);

        Ok(())
    }

    /* /// Negate
    pub fn negate(&mut self) -> () {
        for i in 0..ID_SIZE {
            self.buf[i] = !self.buf[i];
        }

        let mut one = Accumulator::new();
        one.buf[0] = 1u8;
        self.add(&one.buf);
    }

    /// Sub Item
    pub fn sub_item(&mut self, item: &Item) {
        self.sub(&item.id);
    }

    /// Sub Accum
    pub fn sub_accum(&mut self, accum: &Accumulator) {
        self.sub(&accum.buf);
    }

    /// Sub
    pub fn sub(&mut self, buf: &[u8; ID_SIZE]) -> () {
        let mut neg = Accumulator::new();
        neg.buf = *buf;
        neg.negate();
        self.add_accum(&neg);
    } */

    /// Compute fingerprint, given set size
    pub fn get_fingerprint(&self, n: u64) -> Result<Fingerprint, Error> {
        let var_int: Vec<u8> = encode_var_int(n);

        let mut input: Vec<u8> = Vec::with_capacity(ID_SIZE + var_int.len());
        input.extend(&self.buf);
        input.extend(var_int);

        let hash: [u8; 32] = sha256::hash(input);

        Ok(Fingerprint {
            buf: hash[0..FINGERPRINT_SIZE].try_into()?,
        })
    }
}
