use std::{fs::File, path::Path};
use jpeg_decoder::Decoder;

#[test]
fn decoding_in_limited_threadpool_does_not_deadlock() {
    let path = Path::new("tests").join("reftest").join("images").join("mozilla").join("jpg-progressive.jpg");

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| {
        let mut decoder = Decoder::new(File::open(&path).unwrap());
        let _ = decoder.decode().unwrap();
    });
}
