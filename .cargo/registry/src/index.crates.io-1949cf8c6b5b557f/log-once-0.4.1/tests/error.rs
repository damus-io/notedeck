use log::error;
use log_once::error_once;

mod logger;

#[test]
fn error() {
    logger::init();

    for _ in 0..4 {
        error!("Here {}!", 42);
    }

    for _ in 0..4 {
        error_once!("This one is only logged once {}", 43);
        error_once!("This is only logged once too");
    }

    for i in 0..4 {
        error_once!("This will be logged twice {}", i % 2);
    }

    let data = logger::logged_data();
    let expected = "\
Here 42!
Here 42!
Here 42!
Here 42!
This one is only logged once 43
This is only logged once too
This will be logged twice 0
This will be logged twice 1
";
    assert_eq!(data, expected);
}
