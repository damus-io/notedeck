// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use alloc::vec;
use alloc::vec::Vec;
use core::convert::TryInto;

use crate::error::Error;

#[inline]
pub fn get_byte_array<const N: usize>(encoded: &mut &[u8]) -> Result<[u8; N], Error> {
    Ok(get_bytes(encoded, N)?.try_into()?)
}

pub fn get_bytes<'a>(encoded: &'a mut &[u8], n: usize) -> Result<&'a [u8], Error> {
    if encoded.len() < n {
        return Err(Error::ParseEndsPrematurely);
    }
    let res: &[u8] = &encoded[..n];
    *encoded = encoded.get(n..).unwrap_or_default();
    Ok(res)
}

pub fn decode_var_int(encoded: &mut &[u8]) -> Result<u64, Error> {
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

pub fn encode_var_int(mut n: u64) -> Vec<u8> {
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
