use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

pub fn criterion_benchmark(c: &mut Criterion) {
    let cases = [
        "LEHLk~WB2yk8pyo0adR*.7kCMdnj",
        "LGF5]+Yk^6#M@-5c,1J5@[or[Q6.",
        "L6Pj0^jE.AyE_3t7t7R**0o#DgR4",
        "LKO2:N%2Tw=w]~RBVZRi};RPxuwH",
    ];

    for case in cases {
        for size in [50, 100, 200, 256, 500, 512] {
            c.bench_with_input(
                BenchmarkId::new(format!("decode {}", case), size),
                &(case, size),
                |b, &(case, size)| {
                    b.iter(|| {
                        let case = black_box(case);
                        blurhash::decode(case, size, size, 1.0).unwrap()
                    })
                },
            );

            c.bench_with_input(
                BenchmarkId::new(format!("decode_into {}", case), size),
                &(case, size),
                |b, &(case, size)| {
                    let mut buf = vec![0u8; size as usize * size as usize * 4];
                    b.iter(|| {
                        let case = black_box(case);
                        blurhash::decode_into(&mut buf, case, size, size, 1.0).unwrap()
                    })
                },
            );
        }
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
