// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! A very simple serialization framework which is used to serialize/deserialize messages as well
//! as [`ChannelManager`]s and [`ChannelMonitor`]s.
//!
//! [`ChannelManager`]: crate::ln::channelmanager::ChannelManager
//! [`ChannelMonitor`]: crate::chain::channelmonitor::ChannelMonitor

use crate::prelude::*;
use bitcoin::constants::ChainHash;
use core::cmp;
use core::hash::Hash;
use core::ops::Deref;
use std::io::{self, Cursor, Read, Write};
//use std::io_extras::{copy, sink};

//use dnssec_prover::rr::Name;

use crate::ln::msgs::DecodeError;

use crate::util::byte_utils::{be48_to_array, slice_to_be48};

/// serialization buffer size
pub const MAX_BUF_SIZE: usize = 64 * 1024;

/// A simplified version of `std::io::Write` that exists largely for backwards compatibility.
/// An impl is provided for any type that also impls `std::io::Write`.
///
/// This is not exported to bindings users as we only export serialization to/from byte arrays instead
pub trait Writer {
    /// Writes the given buf out. See std::io::Write::write_all for more
    fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error>;
}

impl<W: Write> Writer for W {
    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        <Self as io::Write>::write_all(self, buf)
    }
}

pub(crate) struct VecWriter(pub Vec<u8>);
impl Writer for VecWriter {
    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        self.0.extend_from_slice(buf);
        Ok(())
    }
}

impl Writeable for Vec<u8> {
    #[inline]
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        CollectionLength(self.len() as u64).write(w)?;
        w.write_all(self)
    }
}

impl Readable for Vec<u8> {
    #[inline]
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let mut len: CollectionLength = Readable::read(r)?;
        let mut ret = Vec::new();
        while len.0 > 0 {
            let readamt = cmp::min(len.0 as usize, MAX_BUF_SIZE);
            let readstart = ret.len();
            ret.resize(readstart + readamt, 0);
            r.read_exact(&mut ret[readstart..])?;
            len.0 -= readamt as u64;
        }
        Ok(ret)
    }
}

/// Writer that only tracks the amount of data written - useful if you need to calculate the length
/// of some data when serialized but don't yet need the full data.
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct LengthCalculatingWriter(pub usize);
impl Writer for LengthCalculatingWriter {
    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        self.0 += buf.len();
        Ok(())
    }
}

/// Essentially `std::io::Take` but a bit simpler and with a method to walk the underlying stream
/// forward to ensure we always consume exactly the fixed length specified.
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct FixedLengthReader<'a, R: Read> {
    read: &'a mut R,
    bytes_read: u64,
    total_bytes: u64,
}
impl<'a, R: Read> FixedLengthReader<'a, R> {
    /// Returns a new [`FixedLengthReader`].
    pub fn new(read: &'a mut R, total_bytes: u64) -> Self {
        Self {
            read,
            bytes_read: 0,
            total_bytes,
        }
    }
}
impl<'a, R: Read> Read for FixedLengthReader<'a, R> {
    #[inline]
    fn read(&mut self, dest: &mut [u8]) -> Result<usize, io::Error> {
        if self.total_bytes == self.bytes_read {
            Ok(0)
        } else {
            let read_len = cmp::min(dest.len() as u64, self.total_bytes - self.bytes_read);
            match self.read.read(&mut dest[0..(read_len as usize)]) {
                Ok(v) => {
                    self.bytes_read += v as u64;
                    Ok(v)
                }
                Err(e) => Err(e),
            }
        }
    }
}

impl<'a, R: Read> LengthLimitedRead for FixedLengthReader<'a, R> {
    #[inline]
    fn remaining_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.bytes_read)
    }
}

/// A [`Read`] implementation which tracks whether any bytes have been read at all. This allows us to distinguish
/// between "EOF reached before we started" and "EOF reached mid-read".
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct ReadTrackingReader<'a, R: Read> {
    read: &'a mut R,
    /// Returns whether we have read from this reader or not yet.
    pub have_read: bool,
}
impl<'a, R: Read> ReadTrackingReader<'a, R> {
    /// Returns a new [`ReadTrackingReader`].
    pub fn new(read: &'a mut R) -> Self {
        Self {
            read,
            have_read: false,
        }
    }
}
impl<'a, R: Read> Read for ReadTrackingReader<'a, R> {
    #[inline]
    fn read(&mut self, dest: &mut [u8]) -> Result<usize, io::Error> {
        match self.read.read(dest) {
            Ok(0) => Ok(0),
            Ok(len) => {
                self.have_read = true;
                Ok(len)
            }
            Err(e) => Err(e),
        }
    }
}

/// A trait that various LDK types implement allowing them to be written out to a [`Writer`].
///
/// This is not exported to bindings users as we only export serialization to/from byte arrays instead
pub trait Writeable {
    /// Writes `self` out to the given [`Writer`].
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error>;

    /// Writes `self` out to a `Vec<u8>`.
    fn encode(&self) -> Vec<u8> {
        let len = self.serialized_length();
        let mut msg = VecWriter(Vec::with_capacity(len));
        self.write(&mut msg).unwrap();
        // Note that objects with interior mutability may change size between when we called
        // serialized_length and when we called write. That's okay, but shouldn't happen during
        // testing as most of our tests are not threaded.
        #[cfg(test)]
        debug_assert_eq!(len, msg.0.len());
        msg.0
    }

    /// Writes `self` out to a `Vec<u8>`.
    #[cfg(test)]
    fn encode_with_len(&self) -> Vec<u8> {
        let mut msg = VecWriter(Vec::new());
        0u16.write(&mut msg).unwrap();
        self.write(&mut msg).unwrap();
        let len = msg.0.len();
        debug_assert_eq!(len - 2, self.serialized_length());
        msg.0[..2].copy_from_slice(&(len as u16 - 2).to_be_bytes());
        msg.0
    }

    /// Gets the length of this object after it has been serialized. This can be overridden to
    /// optimize cases where we prepend an object with its length.
    // Note that LLVM optimizes this away in most cases! Check that it isn't before you override!
    #[inline]
    fn serialized_length(&self) -> usize {
        let mut len_calc = LengthCalculatingWriter(0);
        self.write(&mut len_calc)
            .expect("No in-memory data may fail to serialize");
        len_calc.0
    }
}

impl<T: Writeable> Writeable for &T {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        (*self).write(writer)
    }
}

/// A trait that various LDK types implement allowing them to be read in from a [`Read`].
///
/// This is not exported to bindings users as we only export serialization to/from byte arrays instead
pub trait Readable
where
    Self: Sized,
{
    /// Reads a `Self` in from the given [`Read`].
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError>;
}

/// A trait that various higher-level LDK types implement allowing them to be read in
/// from a [`Read`] given some additional set of arguments which is required to deserialize.
///
/// This is not exported to bindings users as we only export serialization to/from byte arrays instead
pub trait ReadableArgs<P>
where
    Self: Sized,
{
    /// Reads a `Self` in from the given [`Read`].
    fn read<R: Read>(reader: &mut R, params: P) -> Result<Self, DecodeError>;
}

/// A [`io::Read`] that limits the amount of bytes that can be read. Implementations should ensure
/// that the object being read will only consume a fixed number of bytes from the underlying
/// [`io::Read`], see [`FixedLengthReader`] for an example.
pub trait LengthLimitedRead: Read {
    /// The number of bytes remaining to be read.
    fn remaining_bytes(&self) -> u64;
}

impl LengthLimitedRead for &[u8] {
    fn remaining_bytes(&self) -> u64 {
        // The underlying `Read` implementation for slice updates the slice to point to the yet unread
        // part.
        self.len() as u64
    }
}

impl LengthLimitedRead for Cursor<&[u8]> {
    fn remaining_bytes(&self) -> u64 {
        let len = self.get_ref().len() as u64;
        let pos = self.position();
        len - pos
    }
}

impl LengthLimitedRead for Cursor<&Vec<u8>> {
    fn remaining_bytes(&self) -> u64 {
        let len = self.get_ref().len() as u64;
        let pos = self.position();
        len - pos
    }
}

/// A trait that allows the implementer to be read in from a [`LengthLimitedRead`], requiring the
/// reader to limit the number of total bytes read from its underlying [`Read`]. Useful for structs
/// that will always consume the entire provided [`Read`] when deserializing.
///
/// Any type that implements [`Readable`] also automatically has a [`LengthReadable`]
/// implementation, but some types, most notably onion packets, only implement [`LengthReadable`].
pub trait LengthReadable
where
    Self: Sized,
{
    /// Reads a `Self` in from the given [`LengthLimitedRead`].
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(
        reader: &mut R,
    ) -> Result<Self, DecodeError>;
}

impl<T: Readable> LengthReadable for T {
    #[inline]
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(
        reader: &mut R,
    ) -> Result<T, DecodeError> {
        Readable::read(reader)
    }
}

impl<T: MaybeReadable> LengthReadable for WithoutLength<Vec<T>> {
    #[inline]
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(
        reader: &mut R,
    ) -> Result<Self, DecodeError> {
        let mut values = Vec::new();
        loop {
            let mut track_read = ReadTrackingReader::new(reader);
            match MaybeReadable::read(&mut track_read) {
                Ok(Some(v)) => {
                    values.push(v);
                }
                Ok(None) => {}
                // If we failed to read any bytes at all, we reached the end of our TLV
                // stream and have simply exhausted all entries.
                Err(ref e) if e == &DecodeError::ShortRead && !track_read.have_read => break,
                Err(e) => return Err(e),
            }
        }
        Ok(Self(values))
    }
}
impl<'a, T> From<&'a Vec<T>> for WithoutLength<&'a Vec<T>> {
    fn from(v: &'a Vec<T>) -> Self {
        Self(v)
    }
}

/// A trait that various LDK types implement allowing them to (maybe) be read in from a [`Read`].
///
/// This is not exported to bindings users as we only export serialization to/from byte arrays instead
pub trait MaybeReadable
where
    Self: Sized,
{
    /// Reads a `Self` in from the given [`Read`].
    fn read<R: Read>(reader: &mut R) -> Result<Option<Self>, DecodeError>;
}

impl<T: Readable> MaybeReadable for T {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<Option<T>, DecodeError> {
        Ok(Some(Readable::read(reader)?))
    }
}

/// Wrapper to read a required (non-optional) TLV record.
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct RequiredWrapper<T>(pub Option<T>);
impl<T: LengthReadable> LengthReadable for RequiredWrapper<T> {
    #[inline]
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(
        reader: &mut R,
    ) -> Result<Self, DecodeError> {
        Ok(Self(Some(LengthReadable::read_from_fixed_length_buffer(
            reader,
        )?)))
    }
}
impl<A, T: ReadableArgs<A>> ReadableArgs<A> for RequiredWrapper<T> {
    #[inline]
    fn read<R: Read>(reader: &mut R, args: A) -> Result<Self, DecodeError> {
        Ok(Self(Some(ReadableArgs::read(reader, args)?)))
    }
}
/// When handling `default_values`, we want to map the default-value T directly
/// to a `RequiredWrapper<T>` in a way that works for `field: T = t;` as
/// well. Thus, we assume `Into<T> for T` does nothing and use that.
impl<T> From<T> for RequiredWrapper<T> {
    fn from(t: T) -> RequiredWrapper<T> {
        RequiredWrapper(Some(t))
    }
}
impl<T: Clone> Clone for RequiredWrapper<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Copy> Copy for RequiredWrapper<T> {}

/// Wrapper to read a required (non-optional) TLV record that may have been upgraded without
/// backwards compat.
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct UpgradableRequired<T: MaybeReadable>(pub Option<T>);
impl<T: MaybeReadable> MaybeReadable for UpgradableRequired<T> {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<Option<Self>, DecodeError> {
        let tlv = MaybeReadable::read(reader)?;
        if let Some(tlv) = tlv {
            return Ok(Some(Self(Some(tlv))));
        }
        Ok(None)
    }
}

pub(crate) struct U48(pub u64);
impl Writeable for U48 {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        writer.write_all(&be48_to_array(self.0))
    }
}
impl Readable for U48 {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<U48, DecodeError> {
        let mut buf = [0; 6];
        reader.read_exact(&mut buf)?;
        Ok(U48(slice_to_be48(&buf)))
    }
}

/// Lightning TLV uses a custom variable-length integer called `BigSize`. It is similar to Bitcoin's
/// variable-length integers except that it is serialized in big-endian instead of little-endian.
///
/// Like Bitcoin's variable-length integer, it exhibits ambiguity in that certain values can be
/// encoded in several different ways, which we must check for at deserialization-time. Thus, if
/// you're looking for an example of a variable-length integer to use for your own project, move
/// along, this is a rather poor design.
#[derive(Clone, Copy, Debug, Hash, PartialOrd, Ord, PartialEq, Eq)]
pub struct BigSize(pub u64);
impl Writeable for BigSize {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        match self.0 {
            0..=0xFC => (self.0 as u8).write(writer),
            0xFD..=0xFFFF => {
                0xFDu8.write(writer)?;
                (self.0 as u16).write(writer)
            }
            0x10000..=0xFFFFFFFF => {
                0xFEu8.write(writer)?;
                (self.0 as u32).write(writer)
            }
            _ => {
                0xFFu8.write(writer)?;
                self.0.write(writer)
            }
        }
    }
}
impl Readable for BigSize {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<BigSize, DecodeError> {
        let n: u8 = Readable::read(reader)?;
        match n {
            0xFF => {
                let x: u64 = Readable::read(reader)?;
                if x < 0x100000000 {
                    Err(DecodeError::InvalidValue)
                } else {
                    Ok(BigSize(x))
                }
            }
            0xFE => {
                let x: u32 = Readable::read(reader)?;
                if x < 0x10000 {
                    Err(DecodeError::InvalidValue)
                } else {
                    Ok(BigSize(x as u64))
                }
            }
            0xFD => {
                let x: u16 = Readable::read(reader)?;
                if x < 0xFD {
                    Err(DecodeError::InvalidValue)
                } else {
                    Ok(BigSize(x as u64))
                }
            }
            n => Ok(BigSize(n as u64)),
        }
    }
}

/// The lightning protocol uses u16s for lengths in most cases. As our serialization framework
/// primarily targets that, we must as well. However, because we may serialize objects that have
/// more than 65K entries, we need to be able to store larger values. Thus, we define a variable
/// length integer here that is backwards-compatible for values < 0xffff. We treat 0xffff as
/// "read eight more bytes".
///
/// To ensure we only have one valid encoding per value, we add 0xffff to values written as eight
/// bytes. Thus, 0xfffe is serialized as 0xfffe, whereas 0xffff is serialized as
/// 0xffff0000000000000000 (i.e. read-eight-bytes then zero).
struct CollectionLength(pub u64);
impl Writeable for CollectionLength {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        if self.0 < 0xffff {
            (self.0 as u16).write(writer)
        } else {
            0xffffu16.write(writer)?;
            (self.0 - 0xffff).write(writer)
        }
    }
}

impl Readable for CollectionLength {
    #[inline]
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let mut val: u64 = <u16 as Readable>::read(r)? as u64;
        if val == 0xffff {
            val = <u64 as Readable>::read(r)?
                .checked_add(0xffff)
                .ok_or(DecodeError::InvalidValue)?;
        }
        Ok(CollectionLength(val))
    }
}

/// In TLV we occasionally send fields which only consist of, or potentially end with, a
/// variable-length integer which is simply truncated by skipping high zero bytes. This type
/// encapsulates such integers implementing [`Readable`]/[`Writeable`] for them.
#[cfg_attr(test, derive(PartialEq, Eq, Debug))]
pub(crate) struct HighZeroBytesDroppedBigSize<T>(pub T);

macro_rules! impl_writeable_primitive {
    ($val_type:ty, $len: expr) => {
        impl Writeable for $val_type {
            #[inline]
            fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
                writer.write_all(&self.to_be_bytes())
            }
        }
        impl Writeable for HighZeroBytesDroppedBigSize<$val_type> {
            #[inline]
            fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
                // Skip any full leading 0 bytes when writing (in BE):
                writer.write_all(&self.0.to_be_bytes()[(self.0.leading_zeros() / 8) as usize..$len])
            }
        }
        impl Readable for $val_type {
            #[inline]
            fn read<R: Read>(reader: &mut R) -> Result<$val_type, DecodeError> {
                let mut buf = [0; $len];
                reader.read_exact(&mut buf)?;
                Ok(<$val_type>::from_be_bytes(buf))
            }
        }
        impl Readable for HighZeroBytesDroppedBigSize<$val_type> {
            #[inline]
            fn read<R: Read>(
                reader: &mut R,
            ) -> Result<HighZeroBytesDroppedBigSize<$val_type>, DecodeError> {
                // We need to accept short reads (read_len == 0) as "EOF" and handle them as simply
                // the high bytes being dropped. To do so, we start reading into the middle of buf
                // and then convert the appropriate number of bytes with extra high bytes out of
                // buf.
                let mut buf = [0; $len * 2];
                let mut read_len = reader.read(&mut buf[$len..])?;
                let mut total_read_len = read_len;
                while read_len != 0 && total_read_len != $len {
                    read_len = reader.read(&mut buf[($len + total_read_len)..])?;
                    total_read_len += read_len;
                }
                if total_read_len == 0 || buf[$len] != 0 {
                    let first_byte = $len - ($len - total_read_len);
                    let mut bytes = [0; $len];
                    bytes.copy_from_slice(&buf[first_byte..first_byte + $len]);
                    Ok(HighZeroBytesDroppedBigSize(<$val_type>::from_be_bytes(
                        bytes,
                    )))
                } else {
                    // If the encoding had extra zero bytes, return a failure even though we know
                    // what they meant (as the TLV test vectors require this)
                    Err(DecodeError::InvalidValue)
                }
            }
        }
        impl From<$val_type> for HighZeroBytesDroppedBigSize<$val_type> {
            fn from(val: $val_type) -> Self {
                Self(val)
            }
        }
    };
}

impl_writeable_primitive!(u128, 16);
impl_writeable_primitive!(u64, 8);
impl_writeable_primitive!(u32, 4);
impl_writeable_primitive!(u16, 2);
impl_writeable_primitive!(i64, 8);
impl_writeable_primitive!(i32, 4);
impl_writeable_primitive!(i16, 2);
impl_writeable_primitive!(i8, 1);

impl Writeable for u8 {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        writer.write_all(&[*self])
    }
}
impl Readable for u8 {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<u8, DecodeError> {
        let mut buf = [0; 1];
        reader.read_exact(&mut buf)?;
        Ok(buf[0])
    }
}

impl Writeable for bool {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        writer.write_all(&[if *self { 1 } else { 0 }])
    }
}
impl Readable for bool {
    #[inline]
    fn read<R: Read>(reader: &mut R) -> Result<bool, DecodeError> {
        let mut buf = [0; 1];
        reader.read_exact(&mut buf)?;
        if buf[0] != 0 && buf[0] != 1 {
            return Err(DecodeError::InvalidValue);
        }
        Ok(buf[0] == 1)
    }
}

macro_rules! impl_array {
    ($size:expr, $ty: ty) => {
        impl Writeable for [$ty; $size] {
            #[inline]
            fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
                let mut out = [0; $size * core::mem::size_of::<$ty>()];
                for (idx, v) in self.iter().enumerate() {
                    let startpos = idx * core::mem::size_of::<$ty>();
                    out[startpos..startpos + core::mem::size_of::<$ty>()]
                        .copy_from_slice(&v.to_be_bytes());
                }
                w.write_all(&out)
            }
        }

        impl Readable for [$ty; $size] {
            #[inline]
            fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
                let mut buf = [0u8; $size * core::mem::size_of::<$ty>()];
                r.read_exact(&mut buf)?;
                let mut res = [0; $size];
                for (idx, v) in res.iter_mut().enumerate() {
                    let startpos = idx * core::mem::size_of::<$ty>();
                    let mut arr = [0; core::mem::size_of::<$ty>()];
                    arr.copy_from_slice(&buf[startpos..startpos + core::mem::size_of::<$ty>()]);
                    *v = <$ty>::from_be_bytes(arr);
                }
                Ok(res)
            }
        }
    };
}

impl_array!(4, u8); // for IPv4
impl_array!(12, u8); // for OnionV2
impl_array!(16, u8); // for IPv6
impl_array!(32, u8); // for channel id & hmac
impl_array!(64, u8); // for ecdsa::Signature and schnorr::Signature
impl_array!(66, u8); // for MuSig2 nonces
impl_array!(1300, u8); // for OnionPacket.hop_data

impl_array!(8, u16);
impl_array!(32, u16);

/// A type for variable-length values within TLV record where the length is encoded as part of the record.
/// Used to prevent encoding the length twice.
///
/// This is not exported to bindings users as manual TLV building is not currently supported in bindings
pub struct WithoutLength<T>(pub T);

impl Writeable for WithoutLength<&String> {
    #[inline]
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        w.write_all(self.0.as_bytes())
    }
}
impl LengthReadable for WithoutLength<String> {
    #[inline]
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        let v: WithoutLength<Vec<u8>> = LengthReadable::read_from_fixed_length_buffer(r)?;
        Ok(Self(
            String::from_utf8(v.0).map_err(|_| DecodeError::InvalidValue)?,
        ))
    }
}
impl<'a> From<&'a String> for WithoutLength<&'a String> {
    fn from(s: &'a String) -> Self {
        Self(s)
    }
}

trait AsWriteableSlice {
    type Inner: Writeable;
    fn as_slice(&self) -> &[Self::Inner];
}

impl<T: Writeable> AsWriteableSlice for &Vec<T> {
    type Inner = T;
    fn as_slice(&self) -> &[T] {
        self
    }
}
impl<T: Writeable> AsWriteableSlice for &[T] {
    type Inner = T;
    fn as_slice(&self) -> &[T] {
        self
    }
}

impl Writeable for ChainHash {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        w.write_all(self.as_bytes())
    }
}

impl Readable for ChainHash {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let buf: [u8; 32] = Readable::read(r)?;
        Ok(ChainHash::from(buf))
    }
}

impl<S: AsWriteableSlice> Writeable for WithoutLength<S> {
    #[inline]
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        for ref v in self.0.as_slice() {
            v.write(writer)?;
        }
        Ok(())
    }
}

impl<T: Writeable> Writeable for Box<T> {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        T::write(&**self, w)
    }
}

impl<T: Readable> Readable for Box<T> {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Box::new(Readable::read(r)?))
    }
}

impl<T: Writeable> Writeable for Option<T> {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match *self {
            None => 0u8.write(w)?,
            Some(ref data) => {
                BigSize(data.serialized_length() as u64 + 1).write(w)?;
                data.write(w)?;
            }
        }
        Ok(())
    }
}

impl<T: LengthReadable> Readable for Option<T> {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let len: BigSize = Readable::read(r)?;
        match len.0 {
            0 => Ok(None),
            len => {
                let mut reader = FixedLengthReader::new(r, len - 1);
                Ok(Some(LengthReadable::read_from_fixed_length_buffer(
                    &mut reader,
                )?))
            }
        }
    }
}

macro_rules! impl_tuple_ser {
	($($i: ident : $type: tt),*) => {
		impl<$($type),*> Readable for ($($type),*)
		where $(
			$type: Readable,
		)*
		{
			fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
				Ok(($(<$type as Readable>::read(r)?),*))
			}
		}

		impl<$($type),*> Writeable for ($($type),*)
		where $(
			$type: Writeable,
		)*
		{
			fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
				let ($($i),*) = self;
				$($i.write(w)?;)*
				Ok(())
			}
		}
	}
}

impl_tuple_ser!(a: A, b: B);
impl_tuple_ser!(a: A, b: B, c: C);
impl_tuple_ser!(a: A, b: B, c: C, d: D);
impl_tuple_ser!(a: A, b: B, c: C, d: D, e: E);
impl_tuple_ser!(a: A, b: B, c: C, d: D, e: E, f: F);
impl_tuple_ser!(a: A, b: B, c: C, d: D, e: E, f: F, g: G);

impl Writeable for () {
    fn write<W: Writer>(&self, _: &mut W) -> Result<(), io::Error> {
        Ok(())
    }
}
impl Readable for () {
    fn read<R: Read>(_r: &mut R) -> Result<Self, DecodeError> {
        Ok(())
    }
}

impl Writeable for String {
    #[inline]
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        CollectionLength(self.len() as u64).write(w)?;
        w.write_all(self.as_bytes())
    }
}
impl Readable for String {
    #[inline]
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let v: Vec<u8> = Readable::read(r)?;
        let ret = String::from_utf8(v).map_err(|_| DecodeError::InvalidValue)?;
        Ok(ret)
    }
}

/// Represents a hostname for serialization purposes.
/// Only the character set and length will be validated.
/// The character set consists of ASCII alphanumeric characters, hyphens, and periods.
/// Its length is guaranteed to be representable by a single byte.
/// This serialization is used by [`BOLT 7`] hostnames.
///
/// [`BOLT 7`]: https://github.com/lightning/bolts/blob/master/07-routing-gossip.md
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Hostname(String);
impl Hostname {
    /// Returns the length of the hostname.
    pub fn len(&self) -> u8 {
        self.0.len() as u8
    }

    /// Check if the chars in `s` are allowed to be included in a [`Hostname`].
    pub(crate) fn str_is_valid_hostname(s: &str) -> bool {
        s.len() <= 255
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    }
}

impl core::fmt::Display for Hostname {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)?;
        Ok(())
    }
}
impl Deref for Hostname {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl From<Hostname> for String {
    fn from(hostname: Hostname) -> Self {
        hostname.0
    }
}
impl TryFrom<Vec<u8>> for Hostname {
    type Error = ();

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if let Ok(s) = String::from_utf8(bytes) {
            Hostname::try_from(s)
        } else {
            Err(())
        }
    }
}
impl TryFrom<String> for Hostname {
    type Error = ();

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Hostname::str_is_valid_hostname(&s) {
            Ok(Hostname(s))
        } else {
            Err(())
        }
    }
}
impl Writeable for Hostname {
    #[inline]
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.len().write(w)?;
        w.write_all(self.as_bytes())
    }
}
impl Readable for Hostname {
    #[inline]
    fn read<R: Read>(r: &mut R) -> Result<Hostname, DecodeError> {
        let len: u8 = Readable::read(r)?;
        let mut vec = vec![0; len.into()];
        r.read_exact(&mut vec)?;
        Hostname::try_from(vec).map_err(|_| DecodeError::InvalidValue)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crate::util::ser::{Hostname, Readable, Writeable};
    use bitcoin::hex::FromHex;
    use std::io;

    #[test]
    fn hostname_conversion() {
        assert_eq!(
            Hostname::try_from(String::from("a-test.com"))
                .unwrap()
                .as_str(),
            "a-test.com"
        );

        assert!(Hostname::try_from(String::from("\"")).is_err());
        assert!(Hostname::try_from(String::from("$")).is_err());
        assert!(Hostname::try_from(String::from("⚡")).is_err());
        let mut large_vec = Vec::with_capacity(256);
        large_vec.resize(256, b'A');
        assert!(Hostname::try_from(String::from_utf8(large_vec).unwrap()).is_err());
    }

    #[test]
    fn hostname_serialization() {
        let hostname = Hostname::try_from(String::from("test")).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        hostname.write(&mut buf).unwrap();
        assert_eq!(
            Hostname::read(&mut buf.as_slice()).unwrap().as_str(),
            "test"
        );
    }

    #[test]
    fn bigsize_encoding_decoding() {
        let values = vec![
            0,
            252,
            253,
            65535,
            65536,
            4294967295,
            4294967296,
            18446744073709551615,
        ];
        let bytes = vec![
            "00",
            "fc",
            "fd00fd",
            "fdffff",
            "fe00010000",
            "feffffffff",
            "ff0000000100000000",
            "ffffffffffffffffff",
        ];
        for i in 0..=7 {
            let mut stream = io::Cursor::new(<Vec<u8>>::from_hex(bytes[i]).unwrap());
            assert_eq!(super::BigSize::read(&mut stream).unwrap().0, values[i]);
            let mut stream = super::VecWriter(Vec::new());
            super::BigSize(values[i]).write(&mut stream).unwrap();
            assert_eq!(stream.0, <Vec<u8>>::from_hex(bytes[i]).unwrap());
        }
        let err_bytes = vec![
            "fd00fc",
            "fe0000ffff",
            "ff00000000ffffffff",
            "fd00",
            "feffff",
            "ffffffffff",
            "fd",
            "fe",
            "ff",
            "",
        ];
        for i in 0..=9 {
            let mut stream = io::Cursor::new(<Vec<u8>>::from_hex(err_bytes[i]).unwrap());
            if i < 3 {
                assert_eq!(
                    super::BigSize::read(&mut stream).err(),
                    Some(crate::ln::msgs::DecodeError::InvalidValue)
                );
            } else {
                assert_eq!(
                    super::BigSize::read(&mut stream).err(),
                    Some(crate::ln::msgs::DecodeError::Io(
                        io::ErrorKind::UnexpectedEof
                    ))
                );
            }
        }
    }
}
