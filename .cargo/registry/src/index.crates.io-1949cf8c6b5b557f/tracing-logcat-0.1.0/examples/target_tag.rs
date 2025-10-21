// To see the logs, run `adb logcat -s target_tag`.

use tracing::{debug, error, info, info_span, trace, warn, Level};
use tracing_logcat::{LogcatMakeWriter, LogcatTag};
use tracing_subscriber::fmt::format::Format;

fn main() {
    let writer =
        LogcatMakeWriter::new(LogcatTag::Target).expect("Failed to initialize logcat writer");

    tracing_subscriber::fmt()
        .event_format(
            Format::default()
                .with_level(false)
                .with_target(false)
                .without_time(),
        )
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(Level::TRACE)
        .init();

    let _span = info_span!("span", foo = "bar").entered();

    trace!("trace!");
    debug!("debug!");
    info!("info!");
    warn!("warn!");
    error!("error!");
}
