#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// hide console window on Windows in release

#[cfg(feature = "memory")]
use re_memory::AccountingAllocator;

#[cfg(feature = "memory")]
#[global_allocator]
static GLOBAL: AccountingAllocator<std::alloc::System> =
    AccountingAllocator::new(std::alloc::System);

use notedeck::{Args, DataPath, DataPathType, Notedeck, NotedeckOptions, RuntimeThreadBudget};
use notedeck_chrome::{setup::generate_native_options, Chrome};
use std::time::Duration;
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Maximum time the headless run loop sleeps between ticks when no wake fires.
///
/// The loop is event-driven: every `egui::Context::request_repaint` on the
/// headless context signals [`Notedeck::headless_waker`], so anything that asks
/// for another frame — the ndb ingester, the remote relay bridge, an app or a
/// worker thread — is served with no polling latency. This cap is only the floor
/// cadence, the safety net for work that asks for no frame at all: promise-based
/// background tasks that resolve off-thread (nip05 / zap verification, media
/// jobs) and any time-based work. So it can stay coarse and let the loop idle
/// cheaply — but a quiet period would sleep forever without it.
const HEADLESS_MAX_IDLE: Duration = Duration::from_secs(1);

fn setup_logging(path: &DataPath) -> Option<WorkerGuard> {
    #[allow(unused_variables)] // need guard to live for lifetime of program
    let (maybe_non_blocking, maybe_guard) = {
        let log_path = path.path(DataPathType::Log);
        // Setup logging to file

        use tracing_appender::{
            non_blocking,
            rolling::{RollingFileAppender, Rotation},
        };

        let file_appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(format!("notedeck-{}", env!("CARGO_PKG_VERSION")))
            .filename_suffix("log")
            .max_log_files(3)
            .build(log_path)
            .expect("failed to initialize rolling file appender");

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

fn resolve_native_title(args_raw: &[String]) -> (String, bool) {
    let (args, _) = Args::parse(args_raw);
    let show_title = args.options.contains(NotedeckOptions::ShowTitle);
    let title = args.title.unwrap_or_else(|| "Damus Notedeck".to_string());
    (title, show_title)
}

// Desktop
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let runtime = RuntimeThreadBudget::from_available_parallelism().build_main_runtime();
    runtime.block_on(async_main());
}

#[cfg(not(target_arch = "wasm32"))]
async fn async_main() {
    #[cfg(feature = "memory")]
    re_memory::accounting_allocator::set_tracking_callstacks(true);

    let base_path = DataPath::default_base_or_cwd();
    let path = DataPath::new(base_path.clone());

    // This guard must be scoped for the duration of the entire program so all logs will be written
    let _guard = setup_logging(&path);

    // Pre-scan for --title so we can set the window title and show the
    // titlebar before eframe creates the window.
    let args_raw: Vec<String> = std::env::args().collect();

    // Headless runtime mode: skip eframe/winit entirely and drive every app's
    // background update() loop from an owned run loop on this Tokio runtime.
    let (parsed, _) = Args::parse(&args_raw);
    if parsed.options.contains(NotedeckOptions::Headless) {
        run_headless(base_path, args_raw).await;
        return;
    }

    let (title, show_title) = resolve_native_title(&args_raw);

    let _res = eframe::run_native(
        &title,
        generate_native_options(path, show_title),
        Box::new(|cc| {
            let args: Vec<String> = std::env::args().collect();
            let ctx = &cc.egui_ctx;

            let mut notedeck = Notedeck::init(ctx, base_path, &args);
            notedeck.setup(ctx);
            let chrome = Chrome::new_with_apps(cc, &args, &mut notedeck)?;
            notedeck.set_app(chrome);

            Ok(Box::new(notedeck))
        }),
    );
}

/// Drive Notedeck headless: no eframe window, no winit event loop, no wgpu
/// surface. Boots the same app roster as the GUI path (via
/// [`Chrome::new_headless`]) and runs every app's background `update()` loop
/// from an owned event-driven loop, exactly as `--all-apps-active` does in the
/// GUI — just without the render pass. Intended for a headless server / SSH run
/// where no display stack is available.
///
/// The loop blocks on [`Notedeck::headless_waker`], the signal every
/// `request_repaint()` on the headless context is routed into, with
/// [`HEADLESS_MAX_IDLE`] as a hard cap so it still ticks periodically (and
/// advances promise-based work) when nothing is asking for a frame. A relay
/// event or a streaming dave session thus wakes it immediately, but an idle
/// process sleeps instead of spinning.
///
/// On SIGINT/SIGTERM the loop breaks so `Notedeck` (and its `AppContext`) drop
/// cleanly, which flushes the remote outbox (`AppContext::drop` -> `remote.flush()`);
/// the relay bridge thread stays alive until that final flush completes.
#[cfg(not(target_arch = "wasm32"))]
async fn run_headless(base_path: std::path::PathBuf, args: Vec<String>) {
    // Windowless context: nothing renders from it, but repaint requests against
    // it are what schedule this loop's work — `Notedeck::init` routes them into
    // the headless waker below.
    let ctx = egui::Context::default();

    let mut notedeck = Notedeck::init(&ctx, base_path, &args);
    notedeck.setup(&ctx);
    let chrome = match Chrome::new_headless(&ctx, &args, &mut notedeck) {
        Ok(chrome) => chrome,
        Err(err) => {
            error!("headless: failed to build chrome: {err}");
            return;
        }
    };
    notedeck.set_app(chrome);

    // Fired by every repaint request on `ctx`; present because we booted with
    // `--headless`. Held for the life of the loop so stored wake permits aren't
    // lost between ticks.
    let wake = notedeck.headless_waker();

    info!(
        "headless: running (event-driven, max idle {}ms), Ctrl-C / SIGTERM to stop",
        HEADLESS_MAX_IDLE.as_millis()
    );

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        // Apply pending background work first (on entry: session restore,
        // private-sync spawn, ...), then sleep until the next wake or the cap.
        notedeck.tick_headless(&ctx);

        let idle = tokio::time::sleep(HEADLESS_MAX_IDLE);
        tokio::select! {
            _ = &mut shutdown => break,
            _ = wait_for_wake(wake.as_deref()) => {}
            _ = idle => {}
        }
    }

    info!("headless: shutting down, flushing remote outbox");
    // Drop `Notedeck` before returning so `AppContext::drop` flushes the outbox
    // while the bridge thread is still alive.
    drop(notedeck);
}

/// Resolve when the headless waker is signalled. `notify_one()` stores a permit
/// if no task is currently waiting, so a wake that lands between ticks isn't
/// lost — the next `notified()` returns immediately. With no waker (never the
/// case under `--headless`) this never resolves, leaving the idle cap and the
/// shutdown signal as the only arms of the loop's `select!`.
#[cfg(not(target_arch = "wasm32"))]
async fn wait_for_wake(wake: Option<&tokio::sync::Notify>) {
    match wake {
        Some(notify) => notify.notified().await,
        None => std::future::pending().await,
    }
}

/// Resolve once the process receives an interrupt/terminate signal. On unix
/// this covers both SIGINT (Ctrl-C) and SIGTERM (e.g. `systemctl stop`); on
/// other platforms it falls back to Ctrl-C only.
#[cfg(not(target_arch = "wasm32"))]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
    use super::{resolve_native_title, Notedeck};
    use notedeck::{Args, DataPath, NotedeckOptions};
    use notedeck_columns::Damus;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn create_tmp_dir() -> PathBuf {
        tempfile::TempDir::new()
            .expect("tmp path")
            .path()
            .to_path_buf()
    }

    fn rmrf(path: impl AsRef<Path>) {
        let _ = std::fs::remove_dir_all(path);
    }

    fn apply_window_builder_for_args(args: &[String]) -> (String, bool, egui::ViewportBuilder) {
        let (title, show_title) = resolve_native_title(args);
        let tempdir = TempDir::new().expect("tmp path");
        let options = notedeck_chrome::setup::generate_native_options(
            DataPath::new(tempdir.path()),
            show_title,
        );
        let builder = (options.window_builder.expect("window builder should exist"))(
            egui::ViewportBuilder::default(),
        );
        (title, show_title, builder)
    }

    /// Ensure dbpath actually sets the dbpath correctly.
    #[tokio::test]
    async fn test_dbpath() {
        let datapath = create_tmp_dir();
        let dbpath = create_tmp_dir();
        let args: Vec<String> = [
            "notedeck-test",
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
        let _app = Notedeck::init(&ctx, &datapath, &args);

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
            "notedeck-test",
            "--testrunner",
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
        let mut notedeck = Notedeck::init(&ctx, &tmpdir, &args);
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

        app_ctx.remote.flush();

        rmrf(tmpdir);
    }

    #[tokio::test]
    async fn test_unknown_args() {
        let tmpdir = create_tmp_dir();
        let npub = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let args: Vec<String> = [
            "notedeck-test",
            "--testrunner",
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
        let mut notedeck = Notedeck::init(&ctx, &tmpdir, &args);
        let mut app_ctx = notedeck.app_context();
        let app = Damus::new(&mut app_ctx, &args);
        app_ctx.remote.flush();
        drop(app_ctx);

        // ensure we recognized all the arguments
        let completely_unrecognized: Vec<String> = notedeck
            .unrecognized_args()
            .intersection(app.unrecognized_args())
            .cloned()
            .collect();
        assert_eq!(completely_unrecognized, ["--unknown-arg"]);

        rmrf(tmpdir);
    }

    #[test]
    fn title_flag_startup_path_matches_args_parse_and_shows_native_titlebar() {
        let args = vec![
            "notedeck".to_string(),
            "--title".to_string(),
            "first".to_string(),
            "--title".to_string(),
            "second".to_string(),
        ];

        let (parsed, unrecognized) = Args::parse(&args[1..]);
        assert!(unrecognized.is_empty());

        let (title, show_title, builder) = apply_window_builder_for_args(&args);

        assert_eq!(title, parsed.title.unwrap());
        assert_eq!(
            show_title,
            parsed.options.contains(NotedeckOptions::ShowTitle)
        );
        assert_eq!(builder.fullsize_content_view, None);
        assert_eq!(builder.titlebar_shown, None);
        assert_eq!(builder.title_shown, None);
    }

    #[test]
    fn missing_or_absent_title_hides_native_titlebar() {
        let cases = [
            vec!["notedeck".to_string()],
            vec!["notedeck".to_string(), "--title".to_string()],
        ];

        for args in cases {
            let (parsed, unrecognized) = Args::parse(&args[1..]);
            assert!(unrecognized.is_empty());

            let (title, show_title, builder) = apply_window_builder_for_args(&args);

            assert_eq!(
                title,
                parsed.title.unwrap_or_else(|| "Damus Notedeck".to_string())
            );
            assert_eq!(
                show_title,
                parsed.options.contains(NotedeckOptions::ShowTitle)
            );
            assert_eq!(builder.fullsize_content_view, Some(true));
            assert_eq!(builder.titlebar_shown, Some(false));
            assert_eq!(builder.title_shown, Some(false));
        }
    }
}
