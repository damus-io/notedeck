use criterion::{black_box, criterion_group, criterion_main, Criterion};
use image::GenericImageView;

pub fn lenna(c: &mut Criterion) {
    for case in ["data/SIPI_Jelly_Beans.tiff", "data/octocat.png"] {
        let img = image::open(case).unwrap();

        let (width, height) = img.dimensions();
        let img = img.to_rgba8();

        c.bench_function(&format!("encode {}", case), |b| {
            b.iter(|| blurhash::encode(4, 3, width, height, black_box(&img)).unwrap());
        });
    }
}

criterion_group!(benches, lenna);
criterion_main!(benches);
