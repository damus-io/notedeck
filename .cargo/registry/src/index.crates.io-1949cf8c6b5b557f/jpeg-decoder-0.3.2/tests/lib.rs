extern crate jpeg_decoder as jpeg;
extern crate png;
extern crate walkdir;

use std::path::Path;
use std::fs::File;

mod common;
mod crashtest;
mod reftest;

#[test]
#[cfg(all(target_family="wasm", target_os="unknown"))]
#[wasm_bindgen_test::wasm_bindgen_test]
fn included_file() {
    const FILE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/reftest/images/mozilla/jpg-progressive.jpg"));

    let mut data = FILE;
    let mut decoder = jpeg::Decoder::new(&mut data);
    let ref_data = decoder.decode().unwrap();
    let ref_info = decoder.info().unwrap();

    let mut data = FILE;
    decoder = jpeg::Decoder::new(&mut data);
    decoder.read_info().unwrap();
    let info = decoder.info().unwrap();
    let data = decoder.decode().unwrap();

    assert_eq!(info, decoder.info().unwrap());
    assert_eq!(info, ref_info);
    assert_eq!(data, ref_data);
}

#[test]
fn read_info() {
    let path = Path::new("tests").join("reftest").join("images").join("mozilla").join("jpg-progressive.jpg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    let ref_data = decoder.decode().unwrap();
    let ref_info = decoder.info().unwrap();

    decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.read_info().unwrap();
    let info = decoder.info().unwrap();
    let data = decoder.decode().unwrap();

    assert_eq!(info, decoder.info().unwrap());
    assert_eq!(info, ref_info);
    assert_eq!(data, ref_data);
}

#[test]
fn read_icc_profile() {
    let path = Path::new("tests")
        .join("reftest")
        .join("images")
        .join("mozilla")
        .join("jpg-srgb-icc.jpg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile().unwrap();
    // "acsp" is a mandatory string in ICC profile headers.
    assert_eq!(&profile[36..40], b"acsp");
}

// Test if chunks are concatenated in the correct order
#[test]
fn read_icc_profile_random_order() {
    let path = Path::new("tests")
        .join("icc")
        .join("icc_chunk_order.jpeg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile().unwrap();

    assert_eq!(profile.len(), 254);

    for i in 1..=254 {
        assert_eq!(profile[i - 1], i as u8);
    }
}

// Check if ICC profiles with invalid chunk number 0 are discarded
#[test]
fn read_icc_profile_seq_no_0() {
    let path = Path::new("tests")
        .join("icc")
        .join("icc_chunk_seq_no_0.jpeg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile();
    assert!(profile.is_none());
}

// Check if ICC profiles with multiple chunks with the same number are discarded
#[test]
fn read_icc_profile_double_seq_no() {
    let path = Path::new("tests")
        .join("icc")
        .join("icc_chunk_double_seq_no.jpeg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile();
    assert!(profile.is_none());
}

// Check if ICC profiles with mismatching number of chunks and total chunk count are discarded
#[test]
fn read_icc_profile_chunk_count_mismatch() {
    let path = Path::new("tests")
        .join("icc")
        .join("icc_chunk_count_mismatch.jpeg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile();
    assert!(profile.is_none());
}

// Check if ICC profiles with missing chunk are discarded
#[test]
fn read_icc_profile_missing_chunk() {
    let path = Path::new("tests")
        .join("icc")
        .join("icc_missing_chunk.jpeg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let profile = decoder.icc_profile();
    assert!(profile.is_none());
}

#[test]
fn read_exif_data() {
    let path = Path::new("tests")
        .join("reftest")
        .join("images")
        .join("ycck.jpg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let exif_data = decoder.exif_data().unwrap();
    // exif data start as a TIFF header
    assert_eq!(&exif_data[0..8], b"\x49\x49\x2A\x00\x08\x00\x00\x00");
}

#[test]
fn read_xmp_data() {
    let path = Path::new("tests")
        .join("reftest")
        .join("images")
        .join("ycck.jpg");

    let mut decoder = jpeg::Decoder::new(File::open(&path).unwrap());
    decoder.decode().unwrap();

    let xmp_data = decoder.xmp_data().unwrap();
    assert_eq!(&xmp_data[0..9], b"<?xpacket");
}
