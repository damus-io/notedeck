// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};

use crate::error::Error;
use crate::{hex, ID_SIZE};

/// Bytes
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id([u8; ID_SIZE]);

impl Deref for Id {
    type Target = [u8; ID_SIZE];

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
    /// Construct from byte array
    #[inline]
    pub fn new(bytes: [u8; ID_SIZE]) -> Self {
        Self(bytes)
    }

    /// Construct from slice
    #[inline]
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        // Check len
        if slice.len() != ID_SIZE {
            return Err(Error::InvalidIdSize);
        }

        // Copy bytes
        let mut bytes: [u8; ID_SIZE] = [0u8; ID_SIZE];
        bytes.copy_from_slice(slice);

        // Construct
        Ok(Self::new(bytes))
    }

    /// Construct from hex
    #[inline]
    pub fn from_hex<T>(data: T) -> Result<Self, Error>
    where
        T: AsRef<[u8]>,
    {
        let bytes: Vec<u8> = hex::decode(data)?;
        Self::from_slice(&bytes)
    }

    /// Consume the [`crate::Bytes`] struct and return a hex-encoded string
    #[inline]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Return the inner value
    #[inline]
    pub fn to_bytes(self) -> [u8; ID_SIZE] {
        self.0
    }

    /// Return reference to the inner value
    #[inline]
    pub fn as_bytes(&self) -> &[u8; ID_SIZE] {
        &self.0
    }
}
