pub use tiff_value::*;

use std::{
    cmp,
    collections::BTreeMap,
    convert::{TryFrom, TryInto},
    io::{self, Seek, Write},
    marker::PhantomData,
    mem,
    num::TryFromIntError,
};

use crate::{
    error::TiffResult,
    tags::{CompressionMethod, ResolutionUnit, Tag},
    TiffError, TiffFormatError,
};

pub mod colortype;
pub mod compression;
mod tiff_value;
mod writer;

use self::colortype::*;
use self::compression::*;
use self::writer::*;

/// Encoder for Tiff and BigTiff files.
///
/// With this type you can get a `DirectoryEncoder` or a `ImageEncoder`
/// to encode Tiff/BigTiff ifd directories with images.
///
/// See `DirectoryEncoder` and `ImageEncoder`.
///
/// # Examples
/// ```
/// # extern crate tiff;
/// # fn main() {
/// # let mut file = std::io::Cursor::new(Vec::new());
/// # let image_data = vec![0; 100*100*3];
/// use tiff::encoder::*;
///
/// // create a standard Tiff file
/// let mut tiff = TiffEncoder::new(&mut file).unwrap();
/// tiff.write_image::<colortype::RGB8>(100, 100, &image_data).unwrap();
///
/// // create a BigTiff file
/// let mut bigtiff = TiffEncoder::new_big(&mut file).unwrap();
/// bigtiff.write_image::<colortype::RGB8>(100, 100, &image_data).unwrap();
///
/// # }
/// ```
pub struct TiffEncoder<W, K: TiffKind = TiffKindStandard> {
    writer: TiffWriter<W>,
    kind: PhantomData<K>,
}

/// Constructor functions to create standard Tiff files.
impl<W: Write + Seek> TiffEncoder<W> {
    /// Creates a new encoder for standard Tiff files.
    ///
    /// To create BigTiff files, use [`new_big`][TiffEncoder::new_big] or
    /// [`new_generic`][TiffEncoder::new_generic].
    pub fn new(writer: W) -> TiffResult<TiffEncoder<W, TiffKindStandard>> {
        TiffEncoder::new_generic(writer)
    }
}

/// Constructor functions to create BigTiff files.
impl<W: Write + Seek> TiffEncoder<W, TiffKindBig> {
    /// Creates a new encoder for BigTiff files.
    ///
    /// To create standard Tiff files, use [`new`][TiffEncoder::new] or
    /// [`new_generic`][TiffEncoder::new_generic].
    pub fn new_big(writer: W) -> TiffResult<Self> {
        TiffEncoder::new_generic(writer)
    }
}

/// Generic functions that are available for both Tiff and BigTiff encoders.
impl<W: Write + Seek, K: TiffKind> TiffEncoder<W, K> {
    /// Creates a new Tiff or BigTiff encoder, inferred from the return type.
    pub fn new_generic(writer: W) -> TiffResult<Self> {
        let mut encoder = TiffEncoder {
            writer: TiffWriter::new(writer),
            kind: PhantomData,
        };

        K::write_header(&mut encoder.writer)?;

        Ok(encoder)
    }

    /// Create a [`DirectoryEncoder`] to encode an ifd directory.
    pub fn new_directory(&mut self) -> TiffResult<DirectoryEncoder<W, K>> {
        DirectoryEncoder::new(&mut self.writer)
    }

    /// Create an [`ImageEncoder`] to encode an image one slice at a time.
    pub fn new_image<C: ColorType>(
        &mut self,
        width: u32,
        height: u32,
    ) -> TiffResult<ImageEncoder<W, C, K, Uncompressed>> {
        let encoder = DirectoryEncoder::new(&mut self.writer)?;
        ImageEncoder::new(encoder, width, height)
    }

    /// Create an [`ImageEncoder`] to encode an image one slice at a time.
    pub fn new_image_with_compression<C: ColorType, D: Compression>(
        &mut self,
        width: u32,
        height: u32,
        compression: D,
    ) -> TiffResult<ImageEncoder<W, C, K, D>> {
        let encoder = DirectoryEncoder::new(&mut self.writer)?;
        ImageEncoder::with_compression(encoder, width, height, compression)
    }

    /// Convenience function to write an entire image from memory.
    pub fn write_image<C: ColorType>(
        &mut self,
        width: u32,
        height: u32,
        data: &[C::Inner],
    ) -> TiffResult<()>
    where
        [C::Inner]: TiffValue,
    {
        let encoder = DirectoryEncoder::new(&mut self.writer)?;
        let image: ImageEncoder<W, C, K> = ImageEncoder::new(encoder, width, height)?;
        image.write_data(data)
    }

    /// Convenience function to write an entire image from memory with a given compression.
    pub fn write_image_with_compression<C: ColorType, D: Compression>(
        &mut self,
        width: u32,
        height: u32,
        compression: D,
        data: &[C::Inner],
    ) -> TiffResult<()>
    where
        [C::Inner]: TiffValue,
    {
        let encoder = DirectoryEncoder::new(&mut self.writer)?;
        let image: ImageEncoder<W, C, K, D> =
            ImageEncoder::with_compression(encoder, width, height, compression)?;
        image.write_data(data)
    }
}

/// Low level interface to encode ifd directories.
///
/// You should call `finish` on this when you are finished with it.
/// Encoding can silently fail while this is dropping.
pub struct DirectoryEncoder<'a, W: 'a + Write + Seek, K: TiffKind> {
    writer: &'a mut TiffWriter<W>,
    dropped: bool,
    // We use BTreeMap to make sure tags are written in correct order
    ifd_pointer_pos: u64,
    ifd: BTreeMap<u16, DirectoryEntry<K::OffsetType>>,
}

impl<'a, W: 'a + Write + Seek, K: TiffKind> DirectoryEncoder<'a, W, K> {
    fn new(writer: &'a mut TiffWriter<W>) -> TiffResult<Self> {
        // the previous word is the IFD offset position
        let ifd_pointer_pos = writer.offset() - mem::size_of::<K::OffsetType>() as u64;
        writer.pad_word_boundary()?; // TODO: Do we need to adjust this for BigTiff?
        Ok(DirectoryEncoder {
            writer,
            dropped: false,
            ifd_pointer_pos,
            ifd: BTreeMap::new(),
        })
    }

    /// Write a single ifd tag.
    pub fn write_tag<T: TiffValue>(&mut self, tag: Tag, value: T) -> TiffResult<()> {
        let mut bytes = Vec::with_capacity(value.bytes());
        {
            let mut writer = TiffWriter::new(&mut bytes);
            value.write(&mut writer)?;
        }

        self.ifd.insert(
            tag.to_u16(),
            DirectoryEntry {
                data_type: <T>::FIELD_TYPE.to_u16(),
                count: value.count().try_into()?,
                data: bytes,
            },
        );

        Ok(())
    }

    fn write_directory(&mut self) -> TiffResult<u64> {
        // Start by writing out all values
        for &mut DirectoryEntry {
            data: ref mut bytes,
            ..
        } in self.ifd.values_mut()
        {
            let data_bytes = mem::size_of::<K::OffsetType>();

            if bytes.len() > data_bytes {
                let offset = self.writer.offset();
                self.writer.write_bytes(bytes)?;
                *bytes = vec![0; data_bytes];
                let mut writer = TiffWriter::new(bytes as &mut [u8]);
                K::write_offset(&mut writer, offset)?;
            } else {
                while bytes.len() < data_bytes {
                    bytes.push(0);
                }
            }
        }

        let offset = self.writer.offset();

        K::write_entry_count(self.writer, self.ifd.len())?;
        for (
            tag,
            DirectoryEntry {
                data_type: field_type,
                count,
                data: offset,
            },
        ) in self.ifd.iter()
        {
            self.writer.write_u16(*tag)?;
            self.writer.write_u16(*field_type)?;
            (*count).write(self.writer)?;
            self.writer.write_bytes(offset)?;
        }

        Ok(offset)
    }

    /// Write some data to the tiff file, the offset of the data is returned.
    ///
    /// This could be used to write tiff strips.
    pub fn write_data<T: TiffValue>(&mut self, value: T) -> TiffResult<u64> {
        let offset = self.writer.offset();
        value.write(self.writer)?;
        Ok(offset)
    }

    /// Provides the number of bytes written by the underlying TiffWriter during the last call.
    fn last_written(&self) -> u64 {
        self.writer.last_written()
    }

    fn finish_internal(&mut self) -> TiffResult<()> {
        let ifd_pointer = self.write_directory()?;
        let curr_pos = self.writer.offset();

        self.writer.goto_offset(self.ifd_pointer_pos)?;
        K::write_offset(self.writer, ifd_pointer)?;
        self.writer.goto_offset(curr_pos)?;
        K::write_offset(self.writer, 0)?;

        self.dropped = true;

        Ok(())
    }

    /// Write out the ifd directory.
    pub fn finish(mut self) -> TiffResult<()> {
        self.finish_internal()
    }
}

impl<'a, W: Write + Seek, K: TiffKind> Drop for DirectoryEncoder<'a, W, K> {
    fn drop(&mut self) {
        if !self.dropped {
            let _ = self.finish_internal();
        }
    }
}

/// Type to encode images strip by strip.
///
/// You should call `finish` on this when you are finished with it.
/// Encoding can silently fail while this is dropping.
///
/// # Examples
/// ```
/// # extern crate tiff;
/// # fn main() {
/// # let mut file = std::io::Cursor::new(Vec::new());
/// # let image_data = vec![0; 100*100*3];
/// use tiff::encoder::*;
/// use tiff::tags::Tag;
///
/// let mut tiff = TiffEncoder::new(&mut file).unwrap();
/// let mut image = tiff.new_image::<colortype::RGB8>(100, 100).unwrap();
///
/// // You can encode tags here
/// image.encoder().write_tag(Tag::Artist, "Image-tiff").unwrap();
///
/// // Strip size can be configured before writing data
/// image.rows_per_strip(2).unwrap();
///
/// let mut idx = 0;
/// while image.next_strip_sample_count() > 0 {
///     let sample_count = image.next_strip_sample_count() as usize;
///     image.write_strip(&image_data[idx..idx+sample_count]).unwrap();
///     idx += sample_count;
/// }
/// image.finish().unwrap();
/// # }
/// ```
/// You can also call write_data function wich will encode by strip and finish
pub struct ImageEncoder<
    'a,
    W: 'a + Write + Seek,
    C: ColorType,
    K: TiffKind,
    D: Compression = Uncompressed,
> {
    encoder: DirectoryEncoder<'a, W, K>,
    strip_idx: u64,
    strip_count: u64,
    row_samples: u64,
    width: u32,
    height: u32,
    rows_per_strip: u64,
    strip_offsets: Vec<K::OffsetType>,
    strip_byte_count: Vec<K::OffsetType>,
    dropped: bool,
    compression: D,
    _phantom: ::std::marker::PhantomData<C>,
}

impl<'a, W: 'a + Write + Seek, T: ColorType, K: TiffKind, D: Compression>
    ImageEncoder<'a, W, T, K, D>
{
    fn new(encoder: DirectoryEncoder<'a, W, K>, width: u32, height: u32) -> TiffResult<Self>
    where
        D: Default,
    {
        Self::with_compression(encoder, width, height, D::default())
    }

    fn with_compression(
        mut encoder: DirectoryEncoder<'a, W, K>,
        width: u32,
        height: u32,
        compression: D,
    ) -> TiffResult<Self> {
        if width == 0 || height == 0 {
            return Err(TiffError::FormatError(TiffFormatError::InvalidDimensions(
                width, height,
            )));
        }

        let row_samples = u64::from(width) * u64::try_from(<T>::BITS_PER_SAMPLE.len())?;
        let row_bytes = row_samples * u64::from(<T::Inner>::BYTE_LEN);

        // Limit the strip size to prevent potential memory and security issues.
        // Also keep the multiple strip handling 'oiled'
        let rows_per_strip = {
            match D::COMPRESSION_METHOD {
                CompressionMethod::PackBits => 1, // Each row must be packed separately. Do not compress across row boundaries
                _ => (1_000_000 + row_bytes - 1) / row_bytes,
            }
        };

        let strip_count = (u64::from(height) + rows_per_strip - 1) / rows_per_strip;

        encoder.write_tag(Tag::ImageWidth, width)?;
        encoder.write_tag(Tag::ImageLength, height)?;
        encoder.write_tag(Tag::Compression, D::COMPRESSION_METHOD.to_u16())?;

        encoder.write_tag(Tag::BitsPerSample, <T>::BITS_PER_SAMPLE)?;
        let sample_format: Vec<_> = <T>::SAMPLE_FORMAT.iter().map(|s| s.to_u16()).collect();
        encoder.write_tag(Tag::SampleFormat, &sample_format[..])?;
        encoder.write_tag(Tag::PhotometricInterpretation, <T>::TIFF_VALUE.to_u16())?;

        encoder.write_tag(Tag::RowsPerStrip, u32::try_from(rows_per_strip)?)?;

        encoder.write_tag(
            Tag::SamplesPerPixel,
            u16::try_from(<T>::BITS_PER_SAMPLE.len())?,
        )?;
        encoder.write_tag(Tag::XResolution, Rational { n: 1, d: 1 })?;
        encoder.write_tag(Tag::YResolution, Rational { n: 1, d: 1 })?;
        encoder.write_tag(Tag::ResolutionUnit, ResolutionUnit::None.to_u16())?;

        Ok(ImageEncoder {
            encoder,
            strip_count,
            strip_idx: 0,
            row_samples,
            rows_per_strip,
            width,
            height,
            strip_offsets: Vec::new(),
            strip_byte_count: Vec::new(),
            dropped: false,
            compression,
            _phantom: ::std::marker::PhantomData,
        })
    }

    /// Number of samples the next strip should have.
    pub fn next_strip_sample_count(&self) -> u64 {
        if self.strip_idx >= self.strip_count {
            return 0;
        }

        let raw_start_row = self.strip_idx * self.rows_per_strip;
        let start_row = cmp::min(u64::from(self.height), raw_start_row);
        let end_row = cmp::min(u64::from(self.height), raw_start_row + self.rows_per_strip);

        (end_row - start_row) * self.row_samples
    }

    /// Write a single strip.
    pub fn write_strip(&mut self, value: &[T::Inner]) -> TiffResult<()>
    where
        [T::Inner]: TiffValue,
    {
        let samples = self.next_strip_sample_count();
        if u64::try_from(value.len())? != samples {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Slice is wrong size for strip",
            )
            .into());
        }

        // Write the (possible compressed) data to the encoder.
        let offset = self.encoder.write_data(value)?;
        let byte_count = self.encoder.last_written() as usize;

        self.strip_offsets.push(K::convert_offset(offset)?);
        self.strip_byte_count.push(byte_count.try_into()?);

        self.strip_idx += 1;
        Ok(())
    }

    /// Write strips from data
    pub fn write_data(mut self, data: &[T::Inner]) -> TiffResult<()>
    where
        [T::Inner]: TiffValue,
    {
        let num_pix = usize::try_from(self.width)?
            .checked_mul(usize::try_from(self.height)?)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Image width * height exceeds usize",
                )
            })?;
        if data.len() < num_pix {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Input data slice is undersized for provided dimensions",
            )
            .into());
        }

        self.encoder
            .writer
            .set_compression(self.compression.get_algorithm());

        let mut idx = 0;
        while self.next_strip_sample_count() > 0 {
            let sample_count = usize::try_from(self.next_strip_sample_count())?;
            self.write_strip(&data[idx..idx + sample_count])?;
            idx += sample_count;
        }

        self.encoder.writer.reset_compression();
        self.finish()?;
        Ok(())
    }

    /// Set image resolution
    pub fn resolution(&mut self, unit: ResolutionUnit, value: Rational) {
        self.encoder
            .write_tag(Tag::ResolutionUnit, unit.to_u16())
            .unwrap();
        self.encoder
            .write_tag(Tag::XResolution, value.clone())
            .unwrap();
        self.encoder.write_tag(Tag::YResolution, value).unwrap();
    }

    /// Set image resolution unit
    pub fn resolution_unit(&mut self, unit: ResolutionUnit) {
        self.encoder
            .write_tag(Tag::ResolutionUnit, unit.to_u16())
            .unwrap();
    }

    /// Set image x-resolution
    pub fn x_resolution(&mut self, value: Rational) {
        self.encoder.write_tag(Tag::XResolution, value).unwrap();
    }

    /// Set image y-resolution
    pub fn y_resolution(&mut self, value: Rational) {
        self.encoder.write_tag(Tag::YResolution, value).unwrap();
    }

    /// Set image number of lines per strip
    ///
    /// This function needs to be called before any calls to `write_data` or
    /// `write_strip` and will return an error otherwise.
    pub fn rows_per_strip(&mut self, value: u32) -> TiffResult<()> {
        if self.strip_idx != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot change strip size after data was written",
            )
            .into());
        }
        // Write tag as 32 bits
        self.encoder.write_tag(Tag::RowsPerStrip, value)?;

        let value: u64 = value as u64;
        self.strip_count = (self.height as u64 + value - 1) / value;
        self.rows_per_strip = value;

        Ok(())
    }

    fn finish_internal(&mut self) -> TiffResult<()> {
        self.encoder
            .write_tag(Tag::StripOffsets, K::convert_slice(&self.strip_offsets))?;
        self.encoder.write_tag(
            Tag::StripByteCounts,
            K::convert_slice(&self.strip_byte_count),
        )?;
        self.dropped = true;

        self.encoder.finish_internal()
    }

    /// Get a reference of the underlying `DirectoryEncoder`
    pub fn encoder(&mut self) -> &mut DirectoryEncoder<'a, W, K> {
        &mut self.encoder
    }

    /// Write out image and ifd directory.
    pub fn finish(mut self) -> TiffResult<()> {
        self.finish_internal()
    }
}

impl<'a, W: Write + Seek, C: ColorType, K: TiffKind, D: Compression> Drop
    for ImageEncoder<'a, W, C, K, D>
{
    fn drop(&mut self) {
        if !self.dropped {
            let _ = self.finish_internal();
        }
    }
}

struct DirectoryEntry<S> {
    data_type: u16,
    count: S,
    data: Vec<u8>,
}

/// Trait to abstract over Tiff/BigTiff differences.
///
/// Implemented for [`TiffKindStandard`] and [`TiffKindBig`].
pub trait TiffKind {
    /// The type of offset fields, `u32` for normal Tiff, `u64` for BigTiff.
    type OffsetType: TryFrom<usize, Error = TryFromIntError> + Into<u64> + TiffValue;

    /// Needed for the `convert_slice` method.
    type OffsetArrayType: ?Sized + TiffValue;

    /// Write the (Big)Tiff header.
    fn write_header<W: Write>(writer: &mut TiffWriter<W>) -> TiffResult<()>;

    /// Convert a file offset to `Self::OffsetType`.
    ///
    /// This returns an error for normal Tiff if the offset is larger than `u32::MAX`.
    fn convert_offset(offset: u64) -> TiffResult<Self::OffsetType>;

    /// Write an offset value to the given writer.
    ///
    /// Like `convert_offset`, this errors if `offset > u32::MAX` for normal Tiff.
    fn write_offset<W: Write>(writer: &mut TiffWriter<W>, offset: u64) -> TiffResult<()>;

    /// Write the IFD entry count field with the given `count` value.
    ///
    /// The entry count field is an `u16` for normal Tiff and `u64` for BigTiff. Errors
    /// if the given `usize` is larger than the representable values.
    fn write_entry_count<W: Write>(writer: &mut TiffWriter<W>, count: usize) -> TiffResult<()>;

    /// Internal helper method for satisfying Rust's type checker.
    ///
    /// The `TiffValue` trait is implemented for both primitive values (e.g. `u8`, `u32`) and
    /// slices of primitive values (e.g. `[u8]`, `[u32]`). However, this is not represented in
    /// the type system, so there is no guarantee that that for all `T: TiffValue` there is also
    /// an implementation of `TiffValue` for `[T]`. This method works around that problem by
    /// providing a conversion from `[T]` to some value that implements `TiffValue`, thereby
    /// making all slices of `OffsetType` usable with `write_tag` and similar methods.
    ///
    /// Implementations of this trait should always set `OffsetArrayType` to `[OffsetType]`.
    fn convert_slice(slice: &[Self::OffsetType]) -> &Self::OffsetArrayType;
}

/// Create a standard Tiff file.
pub struct TiffKindStandard;

impl TiffKind for TiffKindStandard {
    type OffsetType = u32;
    type OffsetArrayType = [u32];

    fn write_header<W: Write>(writer: &mut TiffWriter<W>) -> TiffResult<()> {
        write_tiff_header(writer)?;
        // blank the IFD offset location
        writer.write_u32(0)?;

        Ok(())
    }

    fn convert_offset(offset: u64) -> TiffResult<Self::OffsetType> {
        Ok(Self::OffsetType::try_from(offset)?)
    }

    fn write_offset<W: Write>(writer: &mut TiffWriter<W>, offset: u64) -> TiffResult<()> {
        writer.write_u32(u32::try_from(offset)?)?;
        Ok(())
    }

    fn write_entry_count<W: Write>(writer: &mut TiffWriter<W>, count: usize) -> TiffResult<()> {
        writer.write_u16(u16::try_from(count)?)?;

        Ok(())
    }

    fn convert_slice(slice: &[Self::OffsetType]) -> &Self::OffsetArrayType {
        slice
    }
}

/// Create a BigTiff file.
pub struct TiffKindBig;

impl TiffKind for TiffKindBig {
    type OffsetType = u64;
    type OffsetArrayType = [u64];

    fn write_header<W: Write>(writer: &mut TiffWriter<W>) -> TiffResult<()> {
        write_bigtiff_header(writer)?;
        // blank the IFD offset location
        writer.write_u64(0)?;

        Ok(())
    }

    fn convert_offset(offset: u64) -> TiffResult<Self::OffsetType> {
        Ok(offset)
    }

    fn write_offset<W: Write>(writer: &mut TiffWriter<W>, offset: u64) -> TiffResult<()> {
        writer.write_u64(offset)?;
        Ok(())
    }

    fn write_entry_count<W: Write>(writer: &mut TiffWriter<W>, count: usize) -> TiffResult<()> {
        writer.write_u64(u64::try_from(count)?)?;
        Ok(())
    }

    fn convert_slice(slice: &[Self::OffsetType]) -> &Self::OffsetArrayType {
        slice
    }
}
