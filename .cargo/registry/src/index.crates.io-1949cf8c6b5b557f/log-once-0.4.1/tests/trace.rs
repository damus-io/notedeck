use log::trace;
use log_once::trace_once;

mod logger;

#[test]
fn trace() {
    logger::init();

    for _ in 0..4 {
        trace!("Here {}!", 42);
    }

    for _ in 0..4 {
        trace_once!("This one is only logged once {}", 43);
        trace_once!("This is only logged once too");
    }

    for i in 0..4 {
        trace_once!("This will be logged twice {}", i % 2);
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
