// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Various wrapper types (most around 32-byte arrays) for use in lightning.

use crate::ln::msgs::DecodeError;
use crate::util::ser::{Readable, Writeable, Writer};
use std::io;

#[allow(unused_imports)]
use crate::prelude::*;

use bitcoin::hex::display::impl_fmt_traits;

use core::borrow::Borrow;

/// A unique 32-byte identifier for a channel.
/// Depending on how the ID is generated, several varieties are distinguished
/// (but all are stored as 32 bytes):
///   _v1_ and _temporary_.
/// A _v1_ channel ID is generated based on funding tx outpoint (txid & index).
/// A _temporary_ ID is generated randomly.
/// (Later revocation-point-based _v2_ is a possibility.)
/// The variety (context) is not stored, it is relevant only at creation.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChannelId(pub [u8; 32]);

impl ChannelId {
    /// Generic constructor; create a new channel ID from the provided data.
    /// Use a more specific `*_from_*` constructor when possible.
    pub fn from_bytes(data: [u8; 32]) -> Self {
        Self(data)
    }

    /// Create a channel ID consisting of all-zeros data (e.g. when uninitialized or a placeholder).
    pub fn new_zero() -> Self {
        Self([0; 32])
    }

    /// Check whether ID is consisting of all zeros (uninitialized)
    pub fn is_zero(&self) -> bool {
        self.0[..] == [0; 32]
    }
}

impl Writeable for ChannelId {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.0.write(w)
    }
}

impl Readable for ChannelId {
    fn read<R: io::Read>(r: &mut R) -> Result<Self, DecodeError> {
        let buf: [u8; 32] = Readable::read(r)?;
        Ok(ChannelId(buf))
    }
}

impl Borrow<[u8]> for ChannelId {
    fn borrow(&self) -> &[u8] {
        &self.0[..]
    }
}

impl_fmt_traits! {
    impl fmt_traits for ChannelId {
        const LENGTH: usize = 32;
    }
}
