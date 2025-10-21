use mime_guess2::from_ext;

include!("../src/mime_types.rs");

fn main() {
    // Run registered benchmarks.
    divan::main();
}

/// Benchmark `from_ext` with standard extensions.
#[divan::bench]
fn from_ext_benchmark() {
    for (mime_ext, _) in MIME_TYPES {
        divan::black_box(from_ext(mime_ext).first_raw());
    }
}

/// Benchmark `from_ext` with uppercased extensions.
#[divan::bench]
fn from_ext_uppercase_benchmark() {
    let uppercased: Vec<_> = MIME_TYPES.iter().map(|(s, _)| s.to_uppercase()).collect();

    for mime_ext in uppercased {
        divan::black_box(from_ext(&mime_ext).first_raw());
    }
}
