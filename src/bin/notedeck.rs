#![warn(clippy::all, rust_2018_idioms)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release
use notedeck::app_creation::generate_native_options;
use notedeck::Damus;
use notedeck::FileWriterFactory;

// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;

// Desktop
#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    #[allow(unused_variables)] // need guard to live for lifetime of program
    let (maybe_non_blocking, maybe_guard) =
        if let Ok(log_interactor) = FileWriterFactory::new(notedeck::FileWriterType::Log).build() {
            // Setup logging to file
            use std::panic;

            use tracing::error;
            use tracing_appender::{
                non_blocking,
                rolling::{RollingFileAppender, Rotation},
            };

            let file_appender = RollingFileAppender::new(
                Rotation::DAILY,
                log_interactor.get_directory(),
                format!("notedeck-{}.log", env!("CARGO_PKG_VERSION")),
            );
            panic::set_hook(Box::new(|panic_info| {
                error!("Notedeck panicked: {:?}", panic_info);
            }));

            let (non_blocking, _guard) = non_blocking(file_appender);

            (Some(non_blocking), Some(_guard))
        } else {
            (None, None)
        };

    // Log to stdout (if you run with `RUST_LOG=debug`).
    if let Some(non_blocking_writer) = maybe_non_blocking {
        use tracing::Level;
        use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

        let console_layer = fmt::layer().with_target(true).with_writer(std::io::stdout);

        // Create the file layer (writes to the file)
        let file_layer = fmt::layer()
            .with_ansi(false)
            .with_writer(non_blocking_writer);

        // Set up the subscriber to combine both layers
        tracing_subscriber::registry()
            .with(console_layer)
            .with(file_layer)
            .with(tracing_subscriber::filter::LevelFilter::from_level(
                Level::INFO,
            )) // Set log level
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let _res = eframe::run_native(
        "Damus NoteDeck",
        generate_native_options(),
        Box::new(|cc| Ok(Box::new(Damus::new(cc, ".", std::env::args().collect())))),
    );
}

#[cfg(target_arch = "wasm32")]
pub fn main() {
    // Make sure panics are logged using `console.error`.
    console_error_panic_hook::set_once();

    // Redirect tracing to console.log and friends:
    tracing_wasm::set_as_global_default();

    wasm_bindgen_futures::spawn_local(async {
        let web_options = eframe::WebOptions::default();
        eframe::start_web(
            "the_canvas_id", // hardcode it
            web_options,
            Box::new(|cc| Box::new(Damus::new(cc, "."))),
        )
        .await
        .expect("failed to start eframe");
    });
}
