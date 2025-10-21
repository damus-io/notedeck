//! Function for reading TIFF tags

use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::io::{self, Read, Seek};
use std::mem;
use std::str;

use super::stream::{ByteOrder, EndianReader, SmartReader};
use crate::tags::{Tag, Type};
use crate::{TiffError, TiffFormatError, TiffResult};

use self::Value::{
    Ascii, Byte, Double, Float, Ifd, IfdBig, List, Rational, RationalBig, SRational, SRationalBig,
    Short, Signed, SignedBig, Unsigned, UnsignedBig,
};

#[allow(unused_qualifications)]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Byte(u8),
    Short(u16),
    Signed(i32),
    SignedBig(i64),
    Unsigned(u32),
    UnsignedBig(u64),
    Float(f32),
    Double(f64),
    List(Vec<Value>),
    Rational(u32, u32),
    RationalBig(u64, u64),
    SRational(i32, i32),
    SRationalBig(i64, i64),
    Ascii(String),
    Ifd(u32),
    IfdBig(u64),
}

impl Value {
    pub fn into_u8(self) -> TiffResult<u8> {
        match self {
            Byte(val) => Ok(val),
            val => Err(TiffError::FormatError(TiffFormatError::ByteExpected(val))),
        }
    }

    pub fn into_u16(self) -> TiffResult<u16> {
        match self {
            Short(val) => Ok(val),
            Unsigned(val) => Ok(u16::try_from(val)?),
            UnsignedBig(val) => Ok(u16::try_from(val)?),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u32(self) -> TiffResult<u32> {
        match self {
            Short(val) => Ok(val.into()),
            Unsigned(val) => Ok(val),
            UnsignedBig(val) => Ok(u32::try_from(val)?),
            Ifd(val) => Ok(val),
            IfdBig(val) => Ok(u32::try_from(val)?),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_i32(self) -> TiffResult<i32> {
        match self {
            Signed(val) => Ok(val),
            SignedBig(val) => Ok(i32::try_from(val)?),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u64(self) -> TiffResult<u64> {
        match self {
            Short(val) => Ok(val.into()),
            Unsigned(val) => Ok(val.into()),
            UnsignedBig(val) => Ok(val),
            Ifd(val) => Ok(val.into()),
            IfdBig(val) => Ok(val),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_i64(self) -> TiffResult<i64> {
        match self {
            Signed(val) => Ok(val.into()),
            SignedBig(val) => Ok(val),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_f32(self) -> TiffResult<f32> {
        match self {
            Float(val) => Ok(val),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_f64(self) -> TiffResult<f64> {
        match self {
            Double(val) => Ok(val),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_string(self) -> TiffResult<String> {
        match self {
            Ascii(val) => Ok(val),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u32_vec(self) -> TiffResult<Vec<u32>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_u32()?)
                }
                Ok(new_vec)
            }
            Unsigned(val) => Ok(vec![val]),
            UnsignedBig(val) => Ok(vec![u32::try_from(val)?]),
            Rational(numerator, denominator) => Ok(vec![numerator, denominator]),
            RationalBig(numerator, denominator) => {
                Ok(vec![u32::try_from(numerator)?, u32::try_from(denominator)?])
            }
            Ifd(val) => Ok(vec![val]),
            IfdBig(val) => Ok(vec![u32::try_from(val)?]),
            Ascii(val) => Ok(val.chars().map(u32::from).collect()),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u8_vec(self) -> TiffResult<Vec<u8>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_u8()?)
                }
                Ok(new_vec)
            }
            Byte(val) => Ok(vec![val]),

            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u16_vec(self) -> TiffResult<Vec<u16>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_u16()?)
                }
                Ok(new_vec)
            }
            Short(val) => Ok(vec![val]),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_i32_vec(self) -> TiffResult<Vec<i32>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    match v {
                        SRational(numerator, denominator) => {
                            new_vec.push(numerator);
                            new_vec.push(denominator);
                        }
                        SRationalBig(numerator, denominator) => {
                            new_vec.push(i32::try_from(numerator)?);
                            new_vec.push(i32::try_from(denominator)?);
                        }
                        _ => new_vec.push(v.into_i32()?),
                    }
                }
                Ok(new_vec)
            }
            Signed(val) => Ok(vec![val]),
            SignedBig(val) => Ok(vec![i32::try_from(val)?]),
            SRational(numerator, denominator) => Ok(vec![numerator, denominator]),
            SRationalBig(numerator, denominator) => {
                Ok(vec![i32::try_from(numerator)?, i32::try_from(denominator)?])
            }
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_f32_vec(self) -> TiffResult<Vec<f32>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_f32()?)
                }
                Ok(new_vec)
            }
            Float(val) => Ok(vec![val]),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_f64_vec(self) -> TiffResult<Vec<f64>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_f64()?)
                }
                Ok(new_vec)
            }
            Double(val) => Ok(vec![val]),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_u64_vec(self) -> TiffResult<Vec<u64>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    new_vec.push(v.into_u64()?)
                }
                Ok(new_vec)
            }
            Unsigned(val) => Ok(vec![val.into()]),
            UnsignedBig(val) => Ok(vec![val]),
            Rational(numerator, denominator) => Ok(vec![numerator.into(), denominator.into()]),
            RationalBig(numerator, denominator) => Ok(vec![numerator, denominator]),
            Ifd(val) => Ok(vec![val.into()]),
            IfdBig(val) => Ok(vec![val]),
            Ascii(val) => Ok(val.chars().map(u32::from).map(u64::from).collect()),
            val => Err(TiffError::FormatError(
                TiffFormatError::UnsignedIntegerExpected(val),
            )),
        }
    }

    pub fn into_i64_vec(self) -> TiffResult<Vec<i64>> {
        match self {
            List(vec) => {
                let mut new_vec = Vec::with_capacity(vec.len());
                for v in vec {
                    match v {
                        SRational(numerator, denominator) => {
                            new_vec.push(numerator.into());
                            new_vec.push(denominator.into());
                        }
                        SRationalBig(numerator, denominator) => {
                            new_vec.push(numerator);
                            new_vec.push(denominator);
                        }
                        _ => new_vec.push(v.into_i64()?),
                    }
                }
                Ok(new_vec)
            }
            Signed(val) => Ok(vec![val.into()]),
            SignedBig(val) => Ok(vec![val]),
            SRational(numerator, denominator) => Ok(vec![numerator.into(), denominator.into()]),
            SRationalBig(numerator, denominator) => Ok(vec![numerator, denominator]),
            val => Err(TiffError::FormatError(
                TiffFormatError::SignedIntegerExpected(val),
            )),
        }
    }
}

#[derive(Clone)]
pub struct Entry {
    type_: Type,
    count: u64,
    offset: [u8; 8],
}

impl ::std::fmt::Debug for Entry {
    fn fmt(&self, fmt: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        fmt.write_str(&format!(
            "Entry {{ type_: {:?}, count: {:?}, offset: {:?} }}",
            self.type_, self.count, &self.offset
        ))
    }
}

impl Entry {
    pub fn new(type_: Type, count: u32, offset: [u8; 4]) -> Entry {
        let mut offset = offset.to_vec();
        offset.append(&mut vec![0; 4]);
        Entry::new_u64(type_, count.into(), offset[..].try_into().unwrap())
    }

    pub fn new_u64(type_: Type, count: u64, offset: [u8; 8]) -> Entry {
        Entry {
            type_,
            count,
            offset,
        }
    }

    /// Returns a mem_reader for the offset/value field
    fn r(&self, byte_order: ByteOrder) -> SmartReader<io::Cursor<Vec<u8>>> {
        SmartReader::wrap(io::Cursor::new(self.offset.to_vec()), byte_order)
    }

    pub fn val<R: Read + Seek>(
        &self,
        limits: &super::Limits,
        bigtiff: bool,
        reader: &mut SmartReader<R>,
    ) -> TiffResult<Value> {
        // Case 1: there are no values so we can return immediately.
        if self.count == 0 {
            return Ok(List(Vec::new()));
        }

        let bo = reader.byte_order();

        let tag_size = match self.type_ {
            Type::BYTE | Type::SBYTE | Type::ASCII | Type::UNDEFINED => 1,
            Type::SHORT | Type::SSHORT => 2,
            Type::LONG | Type::SLONG | Type::FLOAT | Type::IFD => 4,
            Type::LONG8
            | Type::SLONG8
            | Type::DOUBLE
            | Type::RATIONAL
            | Type::SRATIONAL
            | Type::IFD8 => 8,
        };

        let value_bytes = match self.count.checked_mul(tag_size) {
            Some(n) => n,
            None => {
                return Err(TiffError::LimitsExceeded);
            }
        };

        // Case 2: there is one value.
        if self.count == 1 {
            // 2a: the value is 5-8 bytes and we're in BigTiff mode.
            if bigtiff && value_bytes > 4 && value_bytes <= 8 {
                return Ok(match self.type_ {
                    Type::LONG8 => UnsignedBig(self.r(bo).read_u64()?),
                    Type::SLONG8 => SignedBig(self.r(bo).read_i64()?),
                    Type::DOUBLE => Double(self.r(bo).read_f64()?),
                    Type::RATIONAL => {
                        let mut r = self.r(bo);
                        Rational(r.read_u32()?, r.read_u32()?)
                    }
                    Type::SRATIONAL => {
                        let mut r = self.r(bo);
                        SRational(r.read_i32()?, r.read_i32()?)
                    }
                    Type::IFD8 => IfdBig(self.r(bo).read_u64()?),
                    Type::BYTE
                    | Type::SBYTE
                    | Type::ASCII
                    | Type::UNDEFINED
                    | Type::SHORT
                    | Type::SSHORT
                    | Type::LONG
                    | Type::SLONG
                    | Type::FLOAT
                    | Type::IFD => unreachable!(),
                });
            }

            // 2b: the value is at most 4 bytes or doesn't fit in the offset field.
            return Ok(match self.type_ {
                Type::BYTE => Unsigned(u32::from(self.offset[0])),
                Type::SBYTE => Signed(i32::from(self.offset[0] as i8)),
                Type::UNDEFINED => Byte(self.offset[0]),
                Type::SHORT => Unsigned(u32::from(self.r(bo).read_u16()?)),
                Type::SSHORT => Signed(i32::from(self.r(bo).read_i16()?)),
                Type::LONG => Unsigned(self.r(bo).read_u32()?),
                Type::SLONG => Signed(self.r(bo).read_i32()?),
                Type::FLOAT => Float(self.r(bo).read_f32()?),
                Type::ASCII => {
                    if self.offset[0] == 0 {
                        Ascii("".to_string())
                    } else {
                        return Err(TiffError::FormatError(TiffFormatError::InvalidTag));
                    }
                }
                Type::LONG8 => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    UnsignedBig(reader.read_u64()?)
                }
                Type::SLONG8 => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    SignedBig(reader.read_i64()?)
                }
                Type::DOUBLE => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    Double(reader.read_f64()?)
                }
                Type::RATIONAL => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    Rational(reader.read_u32()?, reader.read_u32()?)
                }
                Type::SRATIONAL => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    SRational(reader.read_i32()?, reader.read_i32()?)
                }
                Type::IFD => Ifd(self.r(bo).read_u32()?),
                Type::IFD8 => {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?;
                    IfdBig(reader.read_u64()?)
                }
            });
        }

        // Case 3: There is more than one value, but it fits in the offset field.
        if value_bytes <= 4 || bigtiff && value_bytes <= 8 {
            match self.type_ {
                Type::BYTE => return offset_to_bytes(self.count as usize, self),
                Type::SBYTE => return offset_to_sbytes(self.count as usize, self),
                Type::ASCII => {
                    let mut buf = vec![0; self.count as usize];
                    self.r(bo).read_exact(&mut buf)?;
                    if buf.is_ascii() && buf.ends_with(&[0]) {
                        let v = str::from_utf8(&buf)?;
                        let v = v.trim_matches(char::from(0));
                        return Ok(Ascii(v.into()));
                    } else {
                        return Err(TiffError::FormatError(TiffFormatError::InvalidTag));
                    }
                }
                Type::UNDEFINED => {
                    return Ok(List(
                        self.offset[0..self.count as usize]
                            .iter()
                            .map(|&b| Byte(b))
                            .collect(),
                    ));
                }
                Type::SHORT => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Short(r.read_u16()?));
                    }
                    return Ok(List(v));
                }
                Type::SSHORT => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Signed(i32::from(r.read_i16()?)));
                    }
                    return Ok(List(v));
                }
                Type::LONG => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Unsigned(r.read_u32()?));
                    }
                    return Ok(List(v));
                }
                Type::SLONG => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Signed(r.read_i32()?));
                    }
                    return Ok(List(v));
                }
                Type::FLOAT => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Float(r.read_f32()?));
                    }
                    return Ok(List(v));
                }
                Type::IFD => {
                    let mut r = self.r(bo);
                    let mut v = Vec::new();
                    for _ in 0..self.count {
                        v.push(Ifd(r.read_u32()?));
                    }
                    return Ok(List(v));
                }
                Type::LONG8
                | Type::SLONG8
                | Type::RATIONAL
                | Type::SRATIONAL
                | Type::DOUBLE
                | Type::IFD8 => {
                    unreachable!()
                }
            }
        }

        // Case 4: there is more than one value, and it doesn't fit in the offset field.
        match self.type_ {
            // TODO check if this could give wrong results
            // at a different endianess of file/computer.
            Type::BYTE => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                let mut buf = [0; 1];
                reader.read_exact(&mut buf)?;
                Ok(UnsignedBig(u64::from(buf[0])))
            }),
            Type::SBYTE => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(SignedBig(i64::from(reader.read_i8()?)))
            }),
            Type::SHORT => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(UnsignedBig(u64::from(reader.read_u16()?)))
            }),
            Type::SSHORT => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(SignedBig(i64::from(reader.read_i16()?)))
            }),
            Type::LONG => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(Unsigned(reader.read_u32()?))
            }),
            Type::SLONG => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(Signed(reader.read_i32()?))
            }),
            Type::FLOAT => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(Float(reader.read_f32()?))
            }),
            Type::DOUBLE => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(Double(reader.read_f64()?))
            }),
            Type::RATIONAL => {
                self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                    Ok(Rational(reader.read_u32()?, reader.read_u32()?))
                })
            }
            Type::SRATIONAL => {
                self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                    Ok(SRational(reader.read_i32()?, reader.read_i32()?))
                })
            }
            Type::LONG8 => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(UnsignedBig(reader.read_u64()?))
            }),
            Type::SLONG8 => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(SignedBig(reader.read_i64()?))
            }),
            Type::IFD => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(Ifd(reader.read_u32()?))
            }),
            Type::IFD8 => self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                Ok(IfdBig(reader.read_u64()?))
            }),
            Type::UNDEFINED => {
                self.decode_offset(self.count, bo, bigtiff, limits, reader, |reader| {
                    let mut buf = [0; 1];
                    reader.read_exact(&mut buf)?;
                    Ok(Byte(buf[0]))
                })
            }
            Type::ASCII => {
                let n = usize::try_from(self.count)?;
                if n > limits.decoding_buffer_size {
                    return Err(TiffError::LimitsExceeded);
                }

                if bigtiff {
                    reader.goto_offset(self.r(bo).read_u64()?)?
                } else {
                    reader.goto_offset(self.r(bo).read_u32()?.into())?
                }

                let mut out = vec![0; n];
                reader.read_exact(&mut out)?;
                // Strings may be null-terminated, so we trim anything downstream of the null byte
                if let Some(first) = out.iter().position(|&b| b == 0) {
                    out.truncate(first);
                }
                Ok(Ascii(String::from_utf8(out)?))
            }
        }
    }

    #[inline]
    fn decode_offset<R, F>(
        &self,
        value_count: u64,
        bo: ByteOrder,
        bigtiff: bool,
        limits: &super::Limits,
        reader: &mut SmartReader<R>,
        decode_fn: F,
    ) -> TiffResult<Value>
    where
        R: Read + Seek,
        F: Fn(&mut SmartReader<R>) -> TiffResult<Value>,
    {
        let value_count = usize::try_from(value_count)?;
        if value_count > limits.decoding_buffer_size / mem::size_of::<Value>() {
            return Err(TiffError::LimitsExceeded);
        }

        let mut v = Vec::with_capacity(value_count);

        let offset = if bigtiff {
            self.r(bo).read_u64()?
        } else {
            self.r(bo).read_u32()?.into()
        };
        reader.goto_offset(offset)?;

        for _ in 0..value_count {
            v.push(decode_fn(reader)?)
        }
        Ok(List(v))
    }
}

/// Extracts a list of BYTE tags stored in an offset
#[inline]
fn offset_to_bytes(n: usize, entry: &Entry) -> TiffResult<Value> {
    Ok(List(
        entry.offset[0..n]
            .iter()
            .map(|&e| Unsigned(u32::from(e)))
            .collect(),
    ))
}

/// Extracts a list of SBYTE tags stored in an offset
#[inline]
fn offset_to_sbytes(n: usize, entry: &Entry) -> TiffResult<Value> {
    Ok(List(
        entry.offset[0..n]
            .iter()
            .map(|&e| Signed(i32::from(e as i8)))
            .collect(),
    ))
}

/// Type representing an Image File Directory
pub type Directory = HashMap<Tag, Entry>;
