// Copyright (c) 2023 Doug Hoyte
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
use alloc::vec::Vec;
use core::convert::TryFrom;
#[cfg(feature = "std")]
use std::collections::HashSet;

mod bytes;
mod constants;
mod encoding;
mod error;
mod hex;
mod id;
mod sha256;
mod storage;
mod types;

pub use self::bytes::Bytes;
pub use self::constants::{FINGERPRINT_SIZE, ID_SIZE, PROTOCOL_VERSION};
use self::encoding::{decode_var_int, encode_var_int, get_byte_array, get_bytes};
pub use self::error::Error;
pub use self::id::Id;
pub use self::storage::{NegentropyStorageBase, NegentropyStorageVector};
use self::types::Mode;
pub use self::types::{Bound, Item};

const MAX_U64: u64 = u64::MAX;
const BUCKETS: usize = 16;
const DOUBLE_BUCKETS: usize = BUCKETS * 2;

/// Negentropy
pub struct Negentropy<T> {
    storage: T,
    frame_size_limit: u64,
    is_initiator: bool,
    last_timestamp_in: u64,
    last_timestamp_out: u64,
}

impl<T> Negentropy<T>
where
    T: NegentropyStorageBase,
{
    /// Create new [`Negentropy`] instance
    ///
    /// Frame size limit must be `equal to 0` or `greater than 4096`
    pub fn new(storage: T, frame_size_limit: u64) -> Result<Self, Error> {
        if frame_size_limit != 0 && frame_size_limit < 4096 {
            return Err(Error::FrameSizeLimitTooSmall);
        }

        Ok(Self {
            storage,
            frame_size_limit,
            is_initiator: false,
            last_timestamp_in: 0,
            last_timestamp_out: 0,
        })
    }

    /// Initiate reconciliation set
    pub fn initiate(&mut self) -> Result<Bytes, Error> {
        if self.is_initiator {
            return Err(Error::AlreadyBuiltInitialMessage);
        }
        self.is_initiator = true;

        let mut output: Vec<u8> = Vec::new();
        output.push(PROTOCOL_VERSION as u8);

        output.extend(self.split_range(0, self.storage.size()?, Bound::with_timestamp(MAX_U64))?);

        Ok(Bytes::from(output))
    }

    /// Check if this instance has been used to create an initial message
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }

    /// Set Initiator: for resuming initiation flow with a new instance
    pub fn set_initiator(&mut self) {
        self.is_initiator = true;
    }

    /// Reconcile (server method)
    pub fn reconcile(&mut self, query: &Bytes) -> Result<Bytes, Error> {
        if self.is_initiator {
            return Err(Error::Initiator);
        }

        self.reconcile_aux(query, &mut Vec::new(), &mut Vec::new())
    }

    /// Reconcile (client method)
    pub fn reconcile_with_ids(
        &mut self,
        query: &Bytes,
        have_ids: &mut Vec<Id>,
        need_ids: &mut Vec<Id>,
    ) -> Result<Option<Bytes>, Error> {
        if !self.is_initiator {
            return Err(Error::NonInitiator);
        }

        let output: Bytes = self.reconcile_aux(query, have_ids, need_ids)?;
        if output.len() == 1 {
            return Ok(None);
        }

        Ok(Some(output))
    }

    fn reconcile_aux(
        &mut self,
        query: &Bytes,
        have_ids: &mut Vec<Id>,
        need_ids: &mut Vec<Id>,
    ) -> Result<Bytes, Error> {
        self.last_timestamp_in = 0;
        self.last_timestamp_out = 0;

        let mut full_output: Vec<u8> = Vec::with_capacity(1);
        full_output.push(PROTOCOL_VERSION as u8);

        let mut query: &[u8] = query.as_ref();

        let protocol_version: u64 = get_byte_array::<1>(&mut query)?
            .first()
            .copied()
            .map(|b| b as u64)
            .ok_or(Error::ProtocolVersionNotFound)?;

        if !(0x60..=0x6F).contains(&protocol_version) {
            return Err(Error::InvalidProtocolVersion);
        }

        if protocol_version != PROTOCOL_VERSION {
            if self.is_initiator {
                return Err(Error::UnsupportedProtocolVersion);
            } else {
                return Ok(Bytes::from(full_output));
            }
        }

        let storage_size = self.storage.size()?;
        let mut prev_bound: Bound = Bound::new();
        let mut prev_index: usize = 0;
        let mut skip: bool = false;

        while !query.is_empty() {
            let mut o: Vec<u8> = Vec::new();

            let curr_bound: Bound = self.decode_bound(&mut query)?;
            let mode: Mode = self.decode_mode(&mut query)?;

            let lower: usize = prev_index;
            let mut upper: usize =
                self.storage
                    .find_lower_bound(prev_index, storage_size, &curr_bound);

            match mode {
                Mode::Skip => {
                    skip = true;
                }
                Mode::Fingerprint => {
                    let their_fingerprint: [u8; FINGERPRINT_SIZE] = get_byte_array(&mut query)?;
                    let our_fingerprint: [u8; FINGERPRINT_SIZE] =
                        self.storage.fingerprint(lower, upper)?.to_bytes();

                    if their_fingerprint != our_fingerprint {
                        // do_skip
                        if skip {
                            skip = false;
                            o.extend(self.encode_bound(&prev_bound));
                            o.extend(self.encode_mode(Mode::Skip));
                        }

                        o.extend(self.split_range(lower, upper, curr_bound)?);
                    } else {
                        skip = true;
                    }
                }
                Mode::IdList => {
                    let num_ids: u64 = decode_var_int(&mut query)?;

                    #[cfg(feature = "std")]
                    let mut their_elems: HashSet<Id> = HashSet::with_capacity(num_ids as usize);
                    #[cfg(not(feature = "std"))]
                    let mut their_elems: BTreeSet<Id> = BTreeSet::new();

                    for _ in 0..num_ids {
                        let e: [u8; ID_SIZE] = get_byte_array(&mut query)?;
                        their_elems.insert(Id::new(e));
                    }

                    self.storage.iterate(lower, upper, &mut |item: Item, _| {
                        let k: Id = item.id;
                        if !their_elems.contains(&k) {
                            if self.is_initiator {
                                have_ids.push(k);
                            }
                        } else {
                            their_elems.remove(&k);
                        }

                        Ok(true)
                    })?;

                    if self.is_initiator {
                        skip = true;

                        for k in their_elems.into_iter() {
                            need_ids.push(k);
                        }
                    } else {
                        // do_skip
                        if skip {
                            skip = false;
                            o.extend(self.encode_bound(&prev_bound));
                            o.extend(self.encode_mode(Mode::Skip));
                        }

                        let mut response_ids: Vec<u8> = Vec::new();
                        let mut num_response_ids: usize = 0;
                        let mut end_bound = curr_bound;

                        self.storage
                            .iterate(lower, upper, &mut |item: Item, index| {
                                if self.exceeded_frame_size_limit(
                                    full_output.len() + response_ids.len(),
                                ) {
                                    end_bound = Bound::from_item(&item);
                                    upper = index; // shrink upper so that remaining range gets correct fingerprint
                                    return Ok(false);
                                }

                                response_ids.extend(item.id.iter());
                                num_response_ids += 1;
                                Ok(true)
                            })?;

                        o.extend(self.encode_bound(&end_bound));
                        o.extend(self.encode_mode(Mode::IdList));
                        o.extend(encode_var_int(num_response_ids as u64));
                        o.extend(response_ids);

                        full_output.extend(&o);
                        o.clear();
                    }
                }
            }

            if self.exceeded_frame_size_limit(full_output.len() + o.len()) {
                // frameSizeLimit exceeded: Stop range processing and return a fingerprint for the remaining range
                let remaining_fingerprint = self.storage.fingerprint(upper, storage_size)?;

                full_output.extend(self.encode_bound(&Bound::with_timestamp(MAX_U64)));
                full_output.extend(self.encode_mode(Mode::Fingerprint));
                full_output.extend(remaining_fingerprint.iter());
                break;
            } else {
                full_output.extend(o);
            }

            prev_index = upper;
            prev_bound = curr_bound;
        }

        Ok(Bytes::from(full_output))
    }

    fn split_range(
        &mut self,
        lower: usize,
        upper: usize,
        upper_bound: Bound,
    ) -> Result<Vec<u8>, Error> {
        let num_elems: usize = upper - lower;
        let mut o: Vec<u8> = Vec::with_capacity(10 + 10 + num_elems);

        if num_elems < DOUBLE_BUCKETS {
            o.extend(self.encode_bound(&upper_bound));
            o.extend(self.encode_mode(Mode::IdList));

            o.extend(encode_var_int(num_elems as u64));
            self.storage.iterate(lower, upper, &mut |item: Item, _| {
                o.extend(item.id.iter());
                Ok(true)
            })?;
        } else {
            let items_per_bucket: usize = num_elems / BUCKETS;
            let buckets_with_extra: usize = num_elems % BUCKETS;
            let mut curr: usize = lower;

            for i in 0..BUCKETS {
                let bucket_size: usize =
                    items_per_bucket + (if i < buckets_with_extra { 1 } else { 0 });
                let our_fingerprint = self.storage.fingerprint(curr, curr + bucket_size)?;
                curr += bucket_size;

                let next_bound = if curr == upper {
                    upper_bound
                } else {
                    let mut prev_item: Item = Item::with_timestamp(0);
                    let mut curr_item: Item = Item::with_timestamp(0);

                    self.storage
                        .iterate(curr - 1, curr + 1, &mut |item: Item, index| {
                            if index == curr - 1 {
                                prev_item = item;
                            } else {
                                curr_item = item;
                            }

                            Ok(true)
                        })?;

                    self.get_minimal_bound(&prev_item, &curr_item)?
                };

                o.extend(self.encode_bound(&next_bound));
                o.extend(self.encode_mode(Mode::Fingerprint));
                o.extend(our_fingerprint.iter());
            }
        }

        Ok(o)
    }

    fn exceeded_frame_size_limit(&self, n: usize) -> bool {
        self.frame_size_limit != 0 && n > (self.frame_size_limit as usize) - 200
    }

    // Decoding

    fn decode_mode(&self, encoded: &mut &[u8]) -> Result<Mode, Error> {
        let mode = decode_var_int(encoded)?;
        Mode::try_from(mode)
    }

    fn decode_timestamp_in(&mut self, encoded: &mut &[u8]) -> Result<u64, Error> {
        let timestamp: u64 = decode_var_int(encoded)?;
        let mut timestamp = if timestamp == 0 {
            MAX_U64
        } else {
            timestamp - 1
        };
        timestamp = timestamp.saturating_add(self.last_timestamp_in);
        self.last_timestamp_in = timestamp;
        Ok(timestamp)
    }

    fn decode_bound(&mut self, encoded: &mut &[u8]) -> Result<Bound, Error> {
        let timestamp = self.decode_timestamp_in(encoded)?;
        let len: usize = decode_var_int(encoded)? as usize;
        let id: &[u8] = get_bytes(encoded, len)?;
        Bound::with_timestamp_and_id(timestamp, id)
    }

    // Encoding
    fn encode_mode(&self, mode: Mode) -> Vec<u8> {
        encode_var_int(mode.as_u64())
    }

    fn encode_timestamp_out(&mut self, timestamp: u64) -> Vec<u8> {
        if timestamp == MAX_U64 {
            self.last_timestamp_out = MAX_U64;
            return encode_var_int(0);
        }

        let temp: u64 = timestamp;
        let timestamp: u64 = timestamp.saturating_sub(self.last_timestamp_out);
        self.last_timestamp_out = temp;
        encode_var_int(timestamp.saturating_add(1))
    }

    fn encode_bound(&mut self, bound: &Bound) -> Vec<u8> {
        let mut output: Vec<u8> = Vec::new();

        output.extend(self.encode_timestamp_out(bound.item.timestamp));
        output.extend(encode_var_int(bound.id_len as u64));

        let mut bound_slice = bound.item.id.to_vec();
        bound_slice.resize(bound.id_len, 0);
        output.extend(bound_slice);

        output
    }

    fn get_minimal_bound(&self, prev: &Item, curr: &Item) -> Result<Bound, Error> {
        if curr.timestamp != prev.timestamp {
            Ok(Bound::with_timestamp(curr.timestamp))
        } else {
            let mut shared_prefix_bytes: usize = 0;
            let curr_key = curr.id;
            let prev_key = prev.id;

            for i in 0..ID_SIZE {
                if curr_key[i] != prev_key[i] {
                    break;
                }
                shared_prefix_bytes += 1;
            }
            Ok(Bound::with_timestamp_and_id(
                curr.timestamp,
                &curr_key[..shared_prefix_bytes + 1],
            )?)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use self::storage::NegentropyStorageVector;
    use super::*;

    #[test]
    fn test_reconciliation_set() {
        // Client
        let mut storage_client = NegentropyStorageVector::new();
        storage_client
            .insert(
                0,
                Id::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .unwrap(),
            )
            .unwrap();
        storage_client
            .insert(
                1,
                Id::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                    .unwrap(),
            )
            .unwrap();
        storage_client.seal().unwrap();

        let mut client = Negentropy::new(storage_client, 0).unwrap();
        let init_output = client.initiate().unwrap();

        // Relay
        let mut storage_relay = NegentropyStorageVector::new();
        storage_relay
            .insert(
                0,
                Id::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .unwrap(),
            )
            .unwrap();
        storage_relay
            .insert(
                2,
                Id::from_hex("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
                    .unwrap(),
            )
            .unwrap();
        storage_relay
            .insert(
                3,
                Id::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                    .unwrap(),
            )
            .unwrap();
        storage_relay
            .insert(
                5,
                Id::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                    .unwrap(),
            )
            .unwrap();
        storage_relay
            .insert(
                10,
                Id::from_hex("3333333333333333333333333333333333333333333333333333333333333333")
                    .unwrap(),
            )
            .unwrap();
        storage_relay.seal().unwrap();
        let mut relay = Negentropy::new(storage_relay, 0).unwrap();
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
        assert!(have_ids.contains(
            &Id::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
        ));

        // Check need IDs
        #[cfg(feature = "std")]
        need_ids.sort();
        assert_eq!(
            need_ids,
            vec![
                Id::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                    .unwrap(),
                Id::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                    .unwrap(),
                Id::from_hex("3333333333333333333333333333333333333333333333333333333333333333")
                    .unwrap(),
                Id::from_hex("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
                    .unwrap(),
            ]
        )
    }
}

#[cfg(bench)]
mod benches {
    use test::{black_box, Bencher};

    use super::storage::NegentropyStorageVector;
    use super::Bytes;

    #[bench]
    pub fn insert(bh: &mut Bencher) {
        let mut storage_client = NegentropyStorageVector::new();
        bh.iter(|| {
            black_box(
                storage_client.insert(
                    0,
                    Bytes::from_hex(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        });
    }
}
