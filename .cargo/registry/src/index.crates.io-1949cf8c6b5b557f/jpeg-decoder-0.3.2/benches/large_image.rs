extern crate criterion;
extern crate jpeg_decoder;

use criterion::{black_box, Criterion};

use jpeg_decoder as jpeg;

fn read_image(image: &[u8]) -> Vec<u8> {
    jpeg::Decoder::new(black_box(image)).decode().unwrap()
}

fn main() {
    let mut c = Criterion::default().configure_from_args();
    c.bench_function("decode a 2268x1512 JPEG", |b| {
        b.iter(|| read_image(include_bytes!("large_image.jpg")))
    });
    c.final_summary();
}
