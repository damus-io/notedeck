use log::info;
use log_once::info_once;

mod logger;

#[test]
fn info() {
    logger::init();

    for _ in 0..4 {
        info!("Here {}!", 42);
    }

    for _ in 0..4 {
        info_once!("This one is only logged once {}", 43);
        info_once!("This is only logged once too");
    }

    for i in 0..4 {
        info_once!("This will be logged twice {}", i % 2);
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
