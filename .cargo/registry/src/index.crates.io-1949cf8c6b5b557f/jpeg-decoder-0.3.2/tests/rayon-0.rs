//! Must be a separate test because it modifies the _global_ rayon pool.
use std::{fs::File, path::Path};
use jpeg_decoder::Decoder;

#[test]
fn decoding_in_global_pool() {
    let path = Path::new("tests").join("reftest").join("images").join("mozilla").join("jpg-progressive.jpg");

    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .unwrap();

    let mut decoder = Decoder::new(File::open(&path).unwrap());
    let _ = decoder.decode().unwrap();
}
