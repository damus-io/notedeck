use crate::util::*;
use crate::{ImageError, ImageResult, ImageSize};

use std::io::{BufRead, Seek, SeekFrom};

pub fn size<R: BufRead + Seek>(reader: &mut R) -> ImageResult<ImageSize> {
    reader.seek(SeekFrom::Start(0))?;
    //  Read the ftyp header size
    let ftyp_size = read_u32(reader, &Endian::Big)?;

    //  Jump to the first actual box offset
    reader.seek(SeekFrom::Start(ftyp_size.into()))?;

    //  Skip to meta tag which contains all the metadata
    skip_to_tag(reader, b"meta")?;
    read_u32(reader, &Endian::Big)?; //  Meta has a junk value after it
    skip_to_tag(reader, b"iprp")?; //  Find iprp tag

    let mut ipco_size = skip_to_tag(reader, b"ipco")? as usize; //  Find ipco tag

    //  Keep track of the max size of ipco tag
    let mut max_width = 0usize;
    let mut max_height = 0usize;
    let mut found_ispe = false;
    let mut rotation = 0u8;

    while let Ok((tag, size)) = read_tag(reader) {
        //  Size of tag length + tag cannot be under 8 (4 bytes each)
        if size < 8 {
            return Err(ImageError::CorruptedImage);
        }

        //  ispe tag has a junk value followed by width and height as u32
        if tag == "ispe" {
            found_ispe = true;
            read_u32(reader, &Endian::Big)?; //  Discard junk value
            let width = read_u32(reader, &Endian::Big)? as usize;
            let height = read_u32(reader, &Endian::Big)? as usize;

            //  Assign new largest size by area
            if width * height > max_width * max_height {
                max_width = width;
                max_height = height;
            }
        } else if tag == "irot" {
            // irot is 9 bytes total: size, tag, 1 byte for rotation (0-3)
            rotation = read_u8(reader)?;
        } else if size >= ipco_size {
            // If we've gone past the ipco boundary, then break
            break;
        } else {
            // If we're still inside ipco, consume all bytes for
            // the current tag, minus the bytes already read in `read_tag`
            ipco_size -= size;
            reader.seek(SeekFrom::Current(size as i64 - 8))?;
        }
    }

    //  If no ispe found, then we have no actual dimension data to use
    if !found_ispe {
        return Err(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Not enough data").into(),
        );
    }

    //  Rotation can only be 0-3. 1 and 3 are 90 and 270 degrees respectively (anti-clockwise)
    //  If we have 90 or 270 rotation, flip width and height
    if rotation == 1 || rotation == 3 {
        std::mem::swap(&mut max_width, &mut max_height);
    }

    Ok(ImageSize {
        width: max_width,
        height: max_height,
    })
}

pub fn matches(header: &[u8]) -> bool {
    if header.len() < 12 || &header[4..8] != b"ftyp" {
        return false;
    }

    let header_brand = &header[8..12];

    // Since other non-heif files may contain ftype in the header
    // we try to use brands to distinguish image files specifically.
    // List of brands from here: https://mp4ra.org/#/brands
    let valid_brands = [
        // HEIF specific
        b"avci", b"avcs", b"heic", b"heim",
        b"heis", b"heix", b"hevc", b"hevm",
        b"hevs", b"hevx", b"jpeg", b"jpgs",
        b"mif1", b"msf1", b"mif2", b"pred",
        // AVIF specific
        b"avif", b"avio", b"avis", b"MA1A",
        b"MA1B",
    ];

    for brand in valid_brands {
        if brand == header_brand {
            return true;
        }
    }
    
    false
}

fn skip_to_tag<R: BufRead + Seek>(reader: &mut R, tag: &[u8]) -> ImageResult<u32> {
    let mut tag_buf = [0; 4];

    loop {
        let size = read_u32(reader, &Endian::Big)?;
        reader.read_exact(&mut tag_buf)?;

        if tag_buf == tag {
            return Ok(size);
        }

        if size >= 8 {
            reader.seek(SeekFrom::Current(size as i64 - 8))?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid heif box size: {}", size),
            )
            .into());
        }
    }
}
