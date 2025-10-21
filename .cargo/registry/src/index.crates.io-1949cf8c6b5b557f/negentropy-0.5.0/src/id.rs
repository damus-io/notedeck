// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use core::ops::{Deref, DerefMut};

use crate::error::Error;
use crate::ID_SIZE;

/// Bytes
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id([u8; ID_SIZE]);

impl Deref for Id {
    type Target = [u8; Self::LEN];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Id {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Id {
    const LEN: usize = ID_SIZE;

    /// Construct from byte array
    #[deprecated(since = "1.0.0", note = "Use `from_byte_array` instead")]
    pub fn new(bytes: [u8; ID_SIZE]) -> Self {
        Self(bytes)
    }

    /// Construct event ID from 32-byte array
    #[inline]
    pub const fn from_byte_array(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// Construct from slice
    #[inline]
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        // Check len
        if slice.len() != Self::LEN {
            return Err(Error::InvalidIdSize);
        }

        // Copy bytes
        let mut bytes: [u8; Self::LEN] = [0u8; Self::LEN];
        bytes.copy_from_slice(slice);

        // Construct
        Ok(Self::from_byte_array(bytes))
    }

    /// Return the inner value
    #[inline]
    pub fn to_bytes(self) -> [u8; Self::LEN] {
        self.0
    }

    /// Return reference to the inner value
    #[inline]
    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}
