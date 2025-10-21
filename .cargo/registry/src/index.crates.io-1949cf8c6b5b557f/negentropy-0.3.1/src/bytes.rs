// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Deref;

use crate::{hex, Error};

/// Bytes
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(Vec<u8>);

impl Deref for Bytes {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Bytes {
    /// Construct from bytes
    pub fn new<T>(bytes: T) -> Self
    where
        T: AsRef<[u8]>,
    {
        Self::from(bytes.as_ref())
    }

    /// Construct from slice
    pub fn from_slice(slice: &[u8]) -> Self {
        Self::from(slice)
    }

    /// Construct from hex
    pub fn from_hex<T>(data: T) -> Result<Self, Error>
    where
        T: AsRef<[u8]>,
    {
        let bytes: Vec<u8> = hex::decode(data)?;
        Ok(Self::from(bytes))
    }

    /// Consume the [`Bytes`] struct and return a hex-encoded string
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Clone the bytes and return a hex-encoded string
    pub fn as_hex(&self) -> String {
        hex::encode(self.0.clone())
    }

    /// Return the inner value
    pub fn to_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Return reference to the inner value
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
