#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// hide console window on Windows in release

#[cfg(feature = "memory")]
use re_memory::AccountingAllocator;

#[cfg(feature = "memory")]
#[global_allocator]
static GLOBAL: AccountingAllocator<std::alloc::System> =
    AccountingAllocator::new(std::alloc::System);

use notedeck::enostr::Error;
use notedeck::{DataPath, DataPathType, Notedeck};
use notedeck_chrome::{
    setup::{generate_native_options, setup_chrome},
    Chrome, NotedeckApp,
};
use notedeck_columns::Damus;
use notedeck_dave::Dave;
use notedeck_notebook::Notebook;
use tracing::error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

fn setup_logging(path: &DataPath) -> Option<WorkerGuard> {
    #[allow(unused_variables)] // need guard to live for lifetime of program
    let (maybe_non_blocking, maybe_guard) = {
        let log_path = path.path(DataPathType::Log);
        // Setup logging to file

        use tracing_appender::{
            non_blocking,
            rolling::{RollingFileAppender, Rotation},
        };

        let file_appender = RollingFileAppender::new(
            Rotation::DAILY,
            log_path,
            format!("notedeck-{}.log", env!("CARGO_PKG_VERSION")),
        );

        let (non_blocking, _guard) = non_blocking(file_appender);

        (Some(non_blocking), Some(_guard))
    };

    // Log to stdout (if you run with `RUST_LOG=debug`).
    if let Some(non_blocking_writer) = maybe_non_blocking {
        use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

        let console_layer = fmt::layer().with_target(true).with_writer(std::io::stdout);

        // Create the file layer (writes to the file)
        let file_layer = fmt::layer()
            .with_ansi(false)
            .with_writer(non_blocking_writer);

        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("notedeck=info"));

        // Set up the subscriber to combine both layers
        tracing_subscriber::registry()
            .with(console_layer)
            .with(file_layer)
            .with(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    }

    maybe_guard
}

// Desktop
#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    #[cfg(feature = "memory")]
    re_memory::accounting_allocator::set_tracking_callstacks(true);

    let base_path = DataPath::default_base_or_cwd();
    let path = DataPath::new(base_path.clone());

    // This guard must be scoped for the duration of the entire program so all logs will be written
    let _guard = setup_logging(&path);

    let _res = eframe::run_native(
        "Damus Notedeck",
        generate_native_options(path),
        Box::new(|cc| {
            let args: Vec<String> = std::env::args().collect();
            let ctx = &cc.egui_ctx;

            let mut notedeck = Notedeck::new(ctx, base_path, &args);

            let mut chrome = Chrome::new();
            let columns = Damus::new(&mut notedeck.app_context(), &args);
            let dave = Dave::new(cc.wgpu_render_state.as_ref());
            let notebook = Notebook::default();

            setup_chrome(
                ctx,
                notedeck.args(),
                notedeck.theme(),
                notedeck.note_body_font_size(),
                notedeck.zoom_factor(),
            );

            // ensure we recognized all the arguments
            let completely_unrecognized: Vec<String> = notedeck
                .unrecognized_args()
                .intersection(columns.unrecognized_args())
                .cloned()
                .collect();
            if !completely_unrecognized.is_empty() {
                error!("Unrecognized arguments: {:?}", completely_unrecognized);
                return Err(Error::Empty.into());
            }

            chrome.add_app(NotedeckApp::Columns(Box::new(columns)));
            chrome.add_app(NotedeckApp::Dave(Box::new(dave)));
            chrome.add_app(NotedeckApp::Notebook(Box::new(notebook)));

            chrome.set_active(0);

            notedeck.set_app(chrome);

            Ok(Box::new(notedeck))
        }),
    );
}

/*
 * TODO: nostrdb not supported on web
 *
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
*/

#[cfg(test)]
mod tests {
    use super::{Damus, Notedeck};
    use std::path::{Path, PathBuf};

    fn create_tmp_dir() -> PathBuf {
        tempfile::TempDir::new()
            .expect("tmp path")
            .path()
            .to_path_buf()
    }

    fn rmrf(path: impl AsRef<Path>) {
        let _ = std::fs::remove_dir_all(path);
    }

    /// Ensure dbpath actually sets the dbpath correctly.
    #[tokio::test]
    async fn test_dbpath() {
        let datapath = create_tmp_dir();
        let dbpath = create_tmp_dir();
        let args: Vec<String> = [
            "--testrunner",
            "--datapath",
            &datapath.to_str().unwrap(),
            "--dbpath",
            &dbpath.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let ctx = egui::Context::default();
        let _app = Notedeck::new(&ctx, &datapath, &args);

        assert!(Path::new(&dbpath.join("data.mdb")).exists());
        assert!(Path::new(&dbpath.join("lock.mdb")).exists());
        assert!(!Path::new(&datapath.join("db")).exists());

        rmrf(datapath);
        rmrf(dbpath);
    }

    #[tokio::test]
    async fn test_column_args() {
        let tmpdir = create_tmp_dir();
        let npub = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let args: Vec<String> = [
            "--testrunner",
            "--no-keystore",
            "--pub",
            npub,
            "-c",
            "notifications",
            "-c",
            "contacts",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let ctx = egui::Context::default();
        let mut notedeck = Notedeck::new(&ctx, &tmpdir, &args);
        let unrecognized_args = notedeck.unrecognized_args().clone();
        let mut app_ctx = notedeck.app_context();
        let app = Damus::new(&mut app_ctx, &args);

        assert_eq!(app.columns(app_ctx.accounts).columns().len(), 2);

        let tl1 = app
            .columns(app_ctx.accounts)
            .column(0)
            .router()
            .top()
            .timeline_id()
            .unwrap();

        let tl2 = app
            .columns(app_ctx.accounts)
            .column(1)
            .router()
            .top()
            .timeline_id()
            .unwrap();

        assert_eq!(app.timeline_cache.num_timelines(), 2);
        assert!(app.timeline_cache.get(&tl1).is_some());
        assert!(app.timeline_cache.get(&tl2).is_some());

        rmrf(tmpdir);
    }

    #[tokio::test]
    async fn test_unknown_args() {
        let tmpdir = create_tmp_dir();
        let npub = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let args: Vec<String> = [
            "--testrunner",
            "--no-keystore",
            "--unknown-arg", // <-- UNKNOWN
            "--pub",
            npub,
            "-c",
            "notifications",
            "-c",
            "contacts",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let ctx = egui::Context::default();
        let mut notedeck = Notedeck::new(&ctx, &tmpdir, &args);
        let mut app_ctx = notedeck.app_context();
        let app = Damus::new(&mut app_ctx, &args);

        // ensure we recognized all the arguments
        let completely_unrecognized: Vec<String> = notedeck
            .unrecognized_args()
            .intersection(app.unrecognized_args())
            .cloned()
            .collect();
        assert_eq!(completely_unrecognized, ["--unknown-arg"]);

        rmrf(tmpdir);
    }
}
