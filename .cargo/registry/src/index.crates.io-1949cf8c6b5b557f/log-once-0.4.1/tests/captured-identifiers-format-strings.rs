//! Test that we can support capture identifiers in format strings introduced in Rust 1.58.0.
//! See https://blog.rust-lang.org/2022/01/13/Rust-1.58.0.html#captured-identifiers-in-format-strings

mod logger;

#[test]
fn info() {
    logger::init();

    let value = "FOO";

    for _ in 0..2 {
        log::info!("This is logged twice {value}!");
    }

    for _ in 0..2 {
        log_once::info_once!("This is only logged once {value}!");
    }

    for i in 0..2 {
        log_once::info_once!("This will be logged twice {i}!");
    }

    let data = logger::logged_data();
    let expected = "\
This is logged twice FOO!
This is logged twice FOO!
This is only logged once FOO!
This will be logged twice 0!
This will be logged twice 1!
";
    assert_eq!(data, expected);
}
