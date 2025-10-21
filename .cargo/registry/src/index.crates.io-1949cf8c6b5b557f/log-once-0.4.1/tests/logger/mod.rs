use log::{LevelFilter, Metadata, Record};
use std::fmt::Write;
use std::sync::{Mutex, Once};


lazy_static::lazy_static! {
    static ref LOGGED_DATA: Mutex<String> = Mutex::new(String::new());
}

/// A simple in memory logger, using
pub struct MemoryLogger;

impl ::log::Log for MemoryLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let mut buffer = LOGGED_DATA.lock().expect("Mutex has been poisoned");
        writeln!(*buffer, "{}", record.args()).expect("Error while writing");
    }

    fn flush(&self) {}
}

static LOGGER: MemoryLogger = MemoryLogger;

pub fn init() {
    static START: Once = Once::new();
    START.call_once(|| {
        ::log::set_logger(&LOGGER).expect("Could not set the logger");
        ::log::set_max_level(LevelFilter::Trace);
    });
}

pub fn logged_data() -> String {
    LOGGED_DATA
        .lock()
        .expect("Mutex has been poisoned")
        .clone()
}
