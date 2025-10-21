// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

//! Rust implementation of the negentropy set-reconciliation protocol.

#![warn(missing_docs)]
#![cfg_attr(bench, feature(test))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(bench)]
extern crate test;

#[cfg(feature = "std")]
extern crate std;

#[cfg(not(feature = "std"))]
use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::convert::TryFrom;
use core::fmt;
#[cfg(feature = "std")]
use std::collections::HashSet;

mod bytes;
mod hex;
mod sha256;

pub use self::bytes::Bytes;

const PROTOCOL_VERSION_0: u64 = 0x60;
const MAX_U64: u64 = u64::MAX;
const BUCKETS: usize = 16;
const DOUBLE_BUCKETS: usize = BUCKETS * 2;

/// Error
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// ID too big
    IdTooBig,
    /// Invalid ID size
    InvalidIdSize,
    /// IdSizeNotMatch
    IdSizeNotMatch,
    /// Frame size limit too small
    FrameSizeLimitTooSmall,
    /// Not sealed
    NotSealed,
    /// Already sealed
    AlreadySealed,
    /// Already built initial message
    AlreadyBuiltInitialMessage,
    /// Initiator error
    Initiator,
    /// Non-initiator error
    NonInitiator,
    /// Initiate after reconcile
    InitiateAfterReconcile,
    /// Unexpected mode
    UnexpectedMode(u64),
    /// Parse ends prematurely
    ParseEndsPrematurely,
    /// Duplicate item added
    DuplicateItemAdded,
    /// Invalid protocol version
    InvalidProtocolVersion,
    /// Unsupported protocol version
    UnsupportedProtocolVersion,
    /// Unexpected output
    UnexpectedOutput {
        /// Expected output
        expected: String,
        /// Found output
        found: String,
    },
    /// Hex error
    Hex(hex::Error),
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdTooBig => write!(f, "ID too big"),
            Self::InvalidIdSize => write!(f, "Invalid ID size"),
            Self::IdSizeNotMatch => write!(f, "Current item ID not match the client ID size"),
            Self::FrameSizeLimitTooSmall => write!(f, "Frame size limit too small"),
            Self::NotSealed => write!(f, "Not sealed"),
            Self::AlreadySealed => write!(f, "Already sealed"),
            Self::AlreadyBuiltInitialMessage => write!(f, "Already built initial message"),
            Self::Initiator => write!(f, "initiator not asking for have/need IDs"),
            Self::NonInitiator => write!(f, "non-initiator asking for have/need IDs"),
            Self::InitiateAfterReconcile => write!(f, "can't initiate after reconcile"),
            Self::UnexpectedMode(m) => write!(f, "Unexpected mode: {}", m),
            Self::ParseEndsPrematurely => write!(f, "parse ends prematurely"),
            Self::DuplicateItemAdded => write!(f, "duplicate item added"),
            Self::InvalidProtocolVersion => write!(f, "invalid negentropy protocol version byte"),
            Self::UnsupportedProtocolVersion => {
                write!(f, "server does not support our negentropy protocol version")
            }
            Self::UnexpectedOutput { expected, found } => write!(
                f,
                "Unexpected output: expected={}, found={}",
                expected, found
            ),
            Self::Hex(e) => write!(f, "Hex: {}", e),
        }
    }
}

impl From<hex::Error> for Error {
    fn from(e: hex::Error) -> Self {
        Self::Hex(e)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Item {
    timestamp: u64,
    id_size: usize,
    id: [u8; 32],
}

impl Item {
    fn new() -> Self {
        Self::default()
    }

    fn with_timestamp(timestamp: u64) -> Self {
        let mut item = Self::new();
        item.timestamp = timestamp;
        item
    }

    fn with_timestamp_and_id<T>(timestamp: u64, id: T) -> Result<Self, Error>
    where
        T: AsRef<[u8]>,
    {
        let id: &[u8] = id.as_ref();
        let len: usize = id.len();

        if len > 32 {
            return Err(Error::IdTooBig);
        }

        let mut item = Self::new();
        item.timestamp = timestamp;
        item.id_size = len;
        item.id[..len].copy_from_slice(id);

        Ok(item)
    }

    fn id_size(&self) -> usize {
        self.id_size
    }

    fn get_id(&self) -> &[u8] {
        self.id.get(..self.id_size).unwrap_or_default()
    }
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

#[derive(Debug, Clone)]
struct OutputRange {
    start: Item,
    end: Item,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Mode {
    Skip = 0,
    Fingerprint = 1,
    IdList = 2,
    Continuation = 3,
    UnsupportedProtocolVersion = 4,
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
            3 => Ok(Mode::Continuation),
            m => Err(Error::UnexpectedMode(m)),
        }
    }
}

/// Negentropy
#[derive(Debug, Clone)]
pub struct Negentropy {
    id_size: usize,
    frame_size_limit: Option<u64>,
    added_items: Vec<Item>,
    item_timestamps: Vec<u64>,
    item_ids: Vec<u8>,
    sealed: bool,
    is_initiator: bool,
    did_handshake: bool,
    continuation_needed: bool,
    pending_outputs: VecDeque<OutputRange>,
}

impl Negentropy {
    /// Create new [`Negentropy`] instance
    pub fn new(id_size: usize, frame_size_limit: Option<u64>) -> Result<Self, Error> {
        if !(8..=32).contains(&id_size) {
            return Err(Error::InvalidIdSize);
        }

        if let Some(frame_size_limit) = frame_size_limit {
            if frame_size_limit > 0 && frame_size_limit < 4096 {
                return Err(Error::FrameSizeLimitTooSmall);
            }
        }

        Ok(Self {
            id_size,
            frame_size_limit,
            added_items: Vec::new(),
            item_timestamps: Vec::new(),
            item_ids: Vec::new(),
            sealed: false,
            is_initiator: false,
            did_handshake: false,
            continuation_needed: false,
            pending_outputs: VecDeque::new(),
        })
    }

    /// Get current ID size
    pub fn id_size(&self) -> usize {
        self.id_size
    }

    /// Check if current instance it's an initiator
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }

    /// Check if sealed
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Check if need to continue
    pub fn continuation_needed(&self) -> bool {
        self.continuation_needed
    }

    /// Add item
    pub fn add_item(&mut self, created_at: u64, id: Bytes) -> Result<(), Error> {
        if self.is_sealed() {
            return Err(Error::AlreadySealed);
        }

        let id: &[u8] = id.as_ref();
        if id.len() < self.id_size {
            return Err(Error::IdSizeNotMatch);
        }

        let elem: Item = Item::with_timestamp_and_id(created_at, &id[0..self.id_size])?;

        self.added_items.push(elem);
        Ok(())
    }

    fn num_items(&self) -> usize {
        self.item_timestamps.len()
    }

    fn get_item_id(&self, i: usize) -> &[u8] {
        let offset = i * self.id_size;
        &self.item_ids[offset..(offset + self.id_size)]
    }

    fn get_item(&self, i: usize) -> Result<Item, Error> {
        Item::with_timestamp_and_id(self.item_timestamps[i], self.get_item_id(i))
    }

    fn compute_fingerprint(&self, lower: usize, num: usize) -> Vec<u8> {
        let offset = lower * self.id_size;
        sha256::hash(&self.item_ids[offset..(offset + (num * self.id_size))])[0..self.id_size]
            .to_vec()
    }

    /// Seal
    pub fn seal(&mut self) -> Result<(), Error> {
        if self.is_sealed() {
            return Err(Error::AlreadySealed);
        }
        self.sealed = true;

        self.added_items.sort();

        if self.added_items.len() > 1 {
            for i in 0..(self.added_items.len() - 1) {
                if self.added_items[i] == self.added_items[i + 1] {
                    return Err(Error::DuplicateItemAdded);
                }
            }
        }

        self.item_timestamps.reserve_exact(self.added_items.len());
        self.item_ids.reserve_exact(self.added_items.len());

        for item in self.added_items.iter() {
            self.item_timestamps.push(item.timestamp);
            self.item_ids.extend(item.get_id());
        }

        self.added_items.clear();
        self.added_items.shrink_to_fit();

        Ok(())
    }

    /// Initiate reconciliation set
    pub fn initiate(&mut self) -> Result<Bytes, Error> {
        if !self.sealed {
            return Err(Error::NotSealed);
        }

        if self.did_handshake {
            return Err(Error::InitiateAfterReconcile);
        }

        self.is_initiator = true;

        let mut outputs: VecDeque<OutputRange> = VecDeque::new();

        self.split_range(
            0,
            self.num_items(),
            Item::new(),
            Item::with_timestamp(MAX_U64),
            &mut outputs,
        )?;

        self.pending_outputs = outputs;

        self.build_output(true)?.ok_or(Error::UnexpectedOutput {
            expected: String::from("Initiate bytes"),
            found: String::from("None"),
        })
    }

    /// Reconcile (server method)
    pub fn reconcile(&mut self, query: &Bytes) -> Result<Bytes, Error> {
        if self.is_initiator {
            return Err(Error::Initiator);
        }

        let mut query: &[u8] = query.as_ref();

        if !self.did_handshake {
            let protocol_version = self.get_bytes(&mut query, 1)?[0] as u64;

            if !(0x60..=0x6F).contains(&protocol_version) {
                return Err(Error::InvalidProtocolVersion);
            }

            if protocol_version != PROTOCOL_VERSION_0 {
                let mut o: Vec<u8> = Vec::new();
                let mut last_timestamp_out: u64 = 0;
                o.extend(self.encode_bound(
                    &Item::with_timestamp(PROTOCOL_VERSION_0),
                    &mut last_timestamp_out,
                ));
                o.extend(self.encode_mode(Mode::UnsupportedProtocolVersion));
                return Ok(Bytes::from(o));
            }

            self.did_handshake = true;
        }

        self.reconcile_aux(query, &mut Vec::new(), &mut Vec::new())?;

        self.build_output(false)?.ok_or(Error::UnexpectedOutput {
            expected: String::from("Reconcilie bytes"),
            found: String::from("None"),
        })
    }

    /// Reconcile (client method)
    pub fn reconcile_with_ids(
        &mut self,
        query: &Bytes,
        have_ids: &mut Vec<Bytes>,
        need_ids: &mut Vec<Bytes>,
    ) -> Result<Option<Bytes>, Error> {
        if !self.is_initiator {
            return Err(Error::NonInitiator);
        }

        let query: &[u8] = query.as_ref();
        self.reconcile_aux(query, have_ids, need_ids)?;
        self.build_output(false)
    }

    fn reconcile_aux(
        &mut self,
        mut query: &[u8],
        have_ids: &mut Vec<Bytes>,
        need_ids: &mut Vec<Bytes>,
    ) -> Result<(), Error> {
        if !self.sealed {
            return Err(Error::NotSealed);
        }

        self.continuation_needed = false;

        let mut prev_bound: Item = Item::new();
        let mut prev_index: usize = 0;
        let mut last_timestamp_in: u64 = 0;
        let mut outputs: VecDeque<OutputRange> = VecDeque::new();

        while !query.is_empty() {
            let curr_bound: Item = self.decode_bound(&mut query, &mut last_timestamp_in)?;
            let mode: Mode = self.decode_mode(&mut query)?;

            let lower: usize = prev_index;
            let upper: usize = self.find_upper_bound(prev_index, self.num_items(), &curr_bound);

            match mode {
                Mode::Skip => (),
                Mode::Fingerprint => {
                    let their_fingerprint: Vec<u8> = self.get_bytes(&mut query, self.id_size)?;
                    let our_fingerprint: Vec<u8> = self.compute_fingerprint(lower, upper - lower);

                    if their_fingerprint != our_fingerprint {
                        self.split_range(lower, upper, prev_bound, curr_bound, &mut outputs)?;
                    }
                }
                Mode::IdList => {
                    let num_ids: u64 = self.decode_var_int(&mut query)?;
                    #[cfg(feature = "std")]
                    let mut their_elems: HashSet<Vec<u8>> =
                        HashSet::with_capacity(num_ids as usize);
                    #[cfg(not(feature = "std"))]
                    let mut their_elems: BTreeSet<Vec<u8>> = BTreeSet::new();

                    for _ in 0..num_ids {
                        let e: Vec<u8> = self.get_bytes(&mut query, self.id_size)?;
                        their_elems.insert(e);
                    }

                    for i in lower..upper {
                        let k = self.get_item_id(i);
                        if !their_elems.contains(k) {
                            if self.is_initiator {
                                have_ids.push(Bytes::from(k));
                            }
                        } else {
                            their_elems.remove(k);
                        }
                    }

                    if self.is_initiator {
                        for k in their_elems.into_iter() {
                            need_ids.push(Bytes::from(k));
                        }
                    } else {
                        let mut response_have_ids: Vec<&[u8]> = Vec::with_capacity(100);
                        let mut it: usize = lower;
                        let mut did_split: bool = false;
                        let mut split_bound: Item = Item::new();

                        while it < upper {
                            let k: &[u8] = self.get_item_id(it);
                            response_have_ids.push(k);
                            if response_have_ids.len() >= 100 {
                                self.flush_id_list_output(
                                    &mut outputs,
                                    upper,
                                    prev_bound,
                                    &mut did_split,
                                    &mut it,
                                    &mut split_bound,
                                    &curr_bound,
                                    &mut response_have_ids,
                                )?;
                            }
                            it += 1;
                        }
                        self.flush_id_list_output(
                            &mut outputs,
                            upper,
                            prev_bound,
                            &mut did_split,
                            &mut it,
                            &mut split_bound,
                            &curr_bound,
                            &mut response_have_ids,
                        )?;
                    }
                }
                Mode::Continuation => {
                    self.continuation_needed = true;
                }
                Mode::UnsupportedProtocolVersion => {
                    return Err(Error::UnsupportedProtocolVersion);
                }
            }

            prev_index = upper;
            prev_bound = curr_bound;
        }

        while let Some(output) = outputs.pop_back() {
            self.pending_outputs.push_front(output);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_id_list_output(
        &self,
        outputs: &mut VecDeque<OutputRange>,
        upper: usize,
        prev_bound: Item,
        did_split: &mut bool,
        it: &mut usize,
        split_bound: &mut Item,
        curr_bound: &Item,
        response_have_ids: &mut Vec<&[u8]>,
    ) -> Result<(), Error> {
        let len: usize = response_have_ids.len();
        let mut payload: Vec<u8> = Vec::with_capacity(10 + 10 + len);
        payload.extend(self.encode_mode(Mode::IdList));
        payload.extend(self.encode_var_int(len as u64));

        for id in response_have_ids.iter() {
            payload.extend_from_slice(id);
        }

        let next_split_bound: Item = if *it + 1 >= upper {
            *curr_bound
        } else {
            self.get_minimal_bound(&self.get_item(*it)?, &self.get_item(*it + 1)?)?
        };

        outputs.push_back(OutputRange {
            start: if *did_split { *split_bound } else { prev_bound },
            end: next_split_bound,
            payload,
        });

        *split_bound = next_split_bound;
        *did_split = true;

        response_have_ids.clear();

        Ok(())
    }

    fn split_range(
        &self,
        lower: usize,
        upper: usize,
        lower_bound: Item,
        upper_bound: Item,
        outputs: &mut VecDeque<OutputRange>,
    ) -> Result<(), Error> {
        let num_elems: usize = upper - lower;

        if num_elems < DOUBLE_BUCKETS {
            let mut payload: Vec<u8> = Vec::with_capacity(10 + 10 + num_elems);
            payload.extend(self.encode_mode(Mode::IdList));
            payload.extend(self.encode_var_int(num_elems as u64));

            for i in 0..num_elems {
                payload.extend_from_slice(self.get_item_id(lower + i));
            }

            outputs.push_back(OutputRange {
                start: lower_bound,
                end: upper_bound,
                payload,
            });
        } else {
            let items_per_bucket: usize = num_elems / BUCKETS;
            let buckets_with_extra: usize = num_elems % BUCKETS;
            let mut curr: usize = lower;
            let mut prev_bound: Item = self.get_item(lower)?;

            for i in 0..BUCKETS {
                let bucket_size: usize =
                    items_per_bucket + (if i < buckets_with_extra { 1 } else { 0 });
                let our_fingerprint = self.compute_fingerprint(curr, bucket_size);
                curr += bucket_size;

                let mut payload: Vec<u8> = Vec::with_capacity(10 + self.id_size);
                payload.extend(self.encode_mode(Mode::Fingerprint));
                payload.extend(our_fingerprint);

                outputs.push_back(OutputRange {
                    start: if i == 0 { lower_bound } else { prev_bound },
                    end: if curr == upper {
                        upper_bound
                    } else {
                        self.get_minimal_bound(&self.get_item(curr - 1)?, &self.get_item(curr)?)?
                    },
                    payload,
                });

                // TODO: use `.ok_or(Error::SomeError)?` instead
                if let Some(back) = outputs.back() {
                    prev_bound = back.end;
                }
            }

            if let Some(output) = outputs.back_mut() {
                output.end = upper_bound;
            }
        }

        Ok(())
    }

    fn build_output(&mut self, initial_message: bool) -> Result<Option<Bytes>, Error> {
        let mut output: Vec<u8> = Vec::new();
        let mut curr_bound: Item = Item::new();
        let mut last_timestamp_out: u64 = 0;

        if initial_message {
            if self.did_handshake {
                return Err(Error::AlreadyBuiltInitialMessage);
            }
            self.did_handshake = true;
            output.push(PROTOCOL_VERSION_0 as u8);
        }

        self.pending_outputs
            .make_contiguous()
            .sort_by(|a, b| a.start.cmp(&b.start));

        while let Some(p) = self.pending_outputs.front() {
            let mut o: Vec<u8> = Vec::new();

            if p.start < curr_bound {
                break;
            }

            if curr_bound != p.start {
                o.extend(self.encode_bound(&p.start, &mut last_timestamp_out));
                o.extend(self.encode_mode(Mode::Skip));
            }

            o.extend(self.encode_bound(&p.end, &mut last_timestamp_out));
            o.extend(&p.payload);

            if let Some(frame_size_limit) = self.frame_size_limit {
                if frame_size_limit > 0 && output.len() + o.len() > (frame_size_limit - 5) as usize
                {
                    // 5 leaves room for Continuation
                    break;
                }
            }

            output.extend(o);
            curr_bound = p.end;
            self.pending_outputs.pop_front();
        }

        if !self.is_initiator && !self.pending_outputs.is_empty() {
            output.extend(
                &self.encode_bound(&Item::with_timestamp(MAX_U64), &mut last_timestamp_out),
            );
            output.extend(self.encode_mode(Mode::Continuation));
        }

        if self.is_initiator && output.is_empty() && !self.continuation_needed {
            return Ok(None);
        }

        Ok(Some(Bytes::from(output)))
    }

    fn find_upper_bound(&self, mut first: usize, last: usize, value: &Item) -> usize {
        let mut count: usize = last - first;

        while count > 0 {
            let mut it: usize = first;
            let step: usize = count / 2;
            it += step;

            let cond: bool = if value.timestamp == self.item_timestamps[it] {
                &value.id[0..self.id_size] < self.get_item_id(it)
            } else {
                value.timestamp < self.item_timestamps[it]
            };

            if cond {
                count = step;
            } else {
                it += 1;
                first = it;
                count -= step + 1;
            }
        }

        first
    }

    fn get_bytes(&self, encoded: &mut &[u8], n: usize) -> Result<Vec<u8>, Error> {
        if encoded.len() < n {
            return Err(Error::ParseEndsPrematurely);
        }
        let res: Vec<u8> = encoded.get(..n).unwrap_or_default().to_vec();
        *encoded = encoded.get(n..).unwrap_or_default();
        Ok(res)
    }

    fn decode_mode(&self, encoded: &mut &[u8]) -> Result<Mode, Error> {
        let mode = self.decode_var_int(encoded)?;
        Mode::try_from(mode)
    }

    fn decode_var_int(&self, encoded: &mut &[u8]) -> Result<u64, Error> {
        let mut res = 0u64;

        for byte in encoded.iter() {
            *encoded = &encoded[1..];
            res = (res << 7) | (*byte as u64 & 0b0111_1111);
            if (byte & 0b1000_0000) == 0 {
                break;
            }
        }

        Ok(res)
    }

    fn decode_timestamp_in(
        &self,
        encoded: &mut &[u8],
        last_timestamp_in: &mut u64,
    ) -> Result<u64, Error> {
        let timestamp: u64 = self.decode_var_int(encoded)?;
        let mut timestamp = if timestamp == 0 {
            MAX_U64
        } else {
            timestamp - 1
        };
        timestamp = timestamp.saturating_add(*last_timestamp_in);
        *last_timestamp_in = timestamp;
        Ok(timestamp)
    }

    fn decode_bound(
        &self,
        encoded: &mut &[u8],
        last_timestamp_in: &mut u64,
    ) -> Result<Item, Error> {
        let timestamp = self.decode_timestamp_in(encoded, last_timestamp_in)?;
        let len = self.decode_var_int(encoded)?;
        let id = self.get_bytes(encoded, len as usize)?;
        Item::with_timestamp_and_id(timestamp, id)
    }

    fn encode_mode(&self, mode: Mode) -> Vec<u8> {
        self.encode_var_int(mode.as_u64())
    }

    fn encode_var_int(&self, mut n: u64) -> Vec<u8> {
        if n == 0 {
            return vec![0];
        }

        let mut o: Vec<u8> = Vec::with_capacity(10);

        while n > 0 {
            o.push((n & 0x7F) as u8);
            n >>= 7;
        }

        o.reverse();

        for i in 0..(o.len() - 1) {
            o[i] |= 0x80;
        }

        o
    }

    fn encode_timestamp_out(&self, timestamp: u64, last_timestamp_out: &mut u64) -> Vec<u8> {
        if timestamp == MAX_U64 {
            *last_timestamp_out = MAX_U64;
            return self.encode_var_int(0);
        }

        let temp: u64 = timestamp;
        let timestamp: u64 = timestamp.saturating_sub(*last_timestamp_out);
        *last_timestamp_out = temp;
        self.encode_var_int(timestamp.saturating_add(1))
    }

    fn encode_bound(&self, bound: &Item, last_timestamp_out: &mut u64) -> Vec<u8> {
        let mut output: Vec<u8> = Vec::new();
        output.extend(self.encode_timestamp_out(bound.timestamp, last_timestamp_out));
        output.extend(self.encode_var_int(bound.id_size() as u64));
        output.extend(bound.get_id());
        output
    }

    fn get_minimal_bound(&self, prev: &Item, curr: &Item) -> Result<Item, Error> {
        if curr.timestamp != prev.timestamp {
            Ok(Item::with_timestamp(curr.timestamp))
        } else {
            let mut shared_prefix_bytes: usize = 0;
            for i in 0..prev.id_size().min(curr.id_size()) {
                if curr.id[i] != prev.id[i] {
                    break;
                }
                shared_prefix_bytes += 1;
            }
            Item::with_timestamp_and_id(curr.timestamp, &curr.id[..shared_prefix_bytes + 1])
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_reconciliation_set() {
        // Client
        let mut client = Negentropy::new(16, None).unwrap();
        client
            .add_item(
                0,
                Bytes::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            )
            .unwrap();
        client
            .add_item(
                1,
                Bytes::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
            )
            .unwrap();
        client.seal().unwrap();
        let init_output = client.initiate().unwrap();

        // Relay
        let mut relay = Negentropy::new(16, None).unwrap();
        relay
            .add_item(
                0,
                Bytes::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            )
            .unwrap();
        relay
            .add_item(
                2,
                Bytes::from_hex("cccccccccccccccccccccccccccccccc").unwrap(),
            )
            .unwrap();
        relay
            .add_item(
                3,
                Bytes::from_hex("11111111111111111111111111111111").unwrap(),
            )
            .unwrap();
        relay
            .add_item(
                5,
                Bytes::from_hex("22222222222222222222222222222222").unwrap(),
            )
            .unwrap();
        relay
            .add_item(
                10,
                Bytes::from_hex("33333333333333333333333333333333").unwrap(),
            )
            .unwrap();
        relay.seal().unwrap();
        let reconcile_output = relay.reconcile(&init_output).unwrap();

        // Client
        let mut have_ids = Vec::new();
        let mut need_ids = Vec::new();
        let reconcile_output_with_ids = client
            .reconcile_with_ids(&reconcile_output, &mut have_ids, &mut need_ids)
            .unwrap();

        // Check reconcile with IDs output
        assert!(reconcile_output_with_ids.is_none());

        // Check have IDs
        assert!(have_ids.contains(&Bytes::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap()));

        // Check need IDs
        #[cfg(feature = "std")]
        need_ids.sort();
        assert_eq!(
            need_ids,
            vec![
                Bytes::from_hex("11111111111111111111111111111111").unwrap(),
                Bytes::from_hex("22222222222222222222222222222222").unwrap(),
                Bytes::from_hex("33333333333333333333333333333333").unwrap(),
                Bytes::from_hex("cccccccccccccccccccccccccccccccc").unwrap(),
            ]
        )
    }

    #[test]
    fn test_invalid_id_size() {
        assert_eq!(Negentropy::new(33, None).unwrap_err(), Error::InvalidIdSize);

        let mut client = Negentropy::new(16, None).unwrap();
        assert_eq!(
            client
                .add_item(0, Bytes::from_hex("abcdef").unwrap())
                .unwrap_err(),
            Error::IdSizeNotMatch
        );
    }
}

#[cfg(bench)]
mod benches {
    use test::{black_box, Bencher};

    use super::{Bytes, Negentropy};

    const ID_SIZE: usize = 16;
    const FRAME_SIZE_LIMIT: Option<u64> = None;
    const ITEMS_LEN: usize = 100_000;

    #[bench]
    pub fn add_item(bh: &mut Bencher) {
        let mut client = Negentropy::new(ID_SIZE, FRAME_SIZE_LIMIT).unwrap();
        bh.iter(|| {
            black_box(client.add_item(
                0,
                Bytes::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            ))
            .unwrap();
        });
    }

    #[bench]
    pub fn final_reconciliation_100_000_items(bh: &mut Bencher) {
        // Client
        let mut client = Negentropy::new(ID_SIZE, FRAME_SIZE_LIMIT).unwrap();
        for (index, item) in generate_combinations("abc", 32, 2).into_iter().enumerate() {
            client
                .add_item(index as u64, Bytes::from_hex(item).unwrap())
                .unwrap();
        }
        client.seal().unwrap();
        let init_output = client.initiate().unwrap();

        let mut relay = Negentropy::new(ID_SIZE, FRAME_SIZE_LIMIT).unwrap();
        for (index, item) in generate_combinations("abc", 32, ITEMS_LEN)
            .into_iter()
            .enumerate()
        {
            relay
                .add_item(index as u64, Bytes::from_hex(item).unwrap())
                .unwrap();
        }
        relay.seal().unwrap();
        let reconcile_output = relay.reconcile(&init_output).unwrap();

        bh.iter(|| {
            let mut have_ids = Vec::new();
            let mut need_ids = Vec::new();
            black_box(client.reconcile_with_ids(&reconcile_output, &mut have_ids, &mut need_ids))
                .unwrap();
        });
    }

    fn generate_combinations(characters: &str, length: usize, max: usize) -> Vec<String> {
        let mut combinations = Vec::new();
        let mut current = String::new();
        generate_combinations_recursive(
            &mut combinations,
            &mut current,
            characters,
            length,
            0,
            max,
        );
        combinations
    }

    fn generate_combinations_recursive(
        combinations: &mut Vec<String>,
        current: &mut String,
        characters: &str,
        length: usize,
        index: usize,
        max: usize,
    ) {
        if length == 0 {
            combinations.push(current.clone());
            return;
        }

        for char in characters.chars() {
            if combinations.len() < max {
                current.push(char);
                generate_combinations_recursive(
                    combinations,
                    current,
                    characters,
                    length - 1,
                    index + 1,
                    max,
                );
                current.pop();
            } else {
                return;
            }
        }
    }
}
