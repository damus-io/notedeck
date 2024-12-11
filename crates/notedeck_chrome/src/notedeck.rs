#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release
use notedeck_chrome::{
    app_size::AppSizeHandler,
    setup::{generate_native_options, setup_cc},
    theme,
};

use notedeck_columns::Damus;

use notedeck::{
    Accounts, AppContext, Args, DataPath, DataPathType, Directory, FileKeyStorage, ImageCache,
    KeyStorageType, NoteCache, ThemeHandler, UnknownIds,
};

use enostr::RelayPool;
use nostrdb::{Config, Ndb, Transaction};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::{path::PathBuf, str::FromStr};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Our browser app state
struct Notedeck {
    ndb: Ndb,
    img_cache: ImageCache,
    unknown_ids: UnknownIds,
    pool: RelayPool,
    note_cache: NoteCache,
    accounts: Accounts,
    path: DataPath,
    args: Args,
    theme: ThemeHandler,
    tabs: Tabs,
    app_rect_handler: AppSizeHandler,
    egui: egui::Context,
}

struct Tabs {
    app: Option<Rc<RefCell<dyn notedeck::App>>>,
}

impl Tabs {
    pub fn new(app: Option<Rc<RefCell<dyn notedeck::App>>>) -> Self {
        Self { app }
    }
}

impl eframe::App for Notedeck {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        //eframe::set_value(storage, eframe::APP_KEY, self);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // TODO: render chrome

        // render app
        if let Some(app) = &self.tabs.app {
            let app = app.clone();
            app.borrow_mut().update(&mut self.app_context());
        }

        self.app_rect_handler.try_save_app_size(ctx);
    }
}

impl Notedeck {
    pub fn new<P: AsRef<Path>>(ctx: &egui::Context, data_path: P, args: &[String]) -> Self {
        let parsed_args = Args::parse(args);
        let is_mobile = parsed_args
            .is_mobile
            .unwrap_or(notedeck::ui::is_compiled_as_mobile());

        // Some people have been running notedeck in debug, let's catch that!
        if !cfg!(test) && cfg!(debug_assertions) && !parsed_args.debug {
            println!("--- WELCOME TO DAMUS NOTEDECK! ---");
            println!("It looks like are running notedeck in debug mode, unless you are a developer, this is not likely what you want.");
            println!("If you are a developer, run `cargo run -- --debug` to skip this message.");
            println!("For everyone else, try again with `cargo run --release`. Enjoy!");
            println!("---------------------------------");
            panic!();
        }

        setup_cc(ctx, is_mobile, parsed_args.light);

        let data_path = parsed_args
            .datapath
            .unwrap_or(data_path.as_ref().to_str().expect("db path ok").to_string());
        let path = DataPath::new(&data_path);
        let dbpath_str = parsed_args
            .dbpath
            .unwrap_or_else(|| path.path(DataPathType::Db).to_str().unwrap().to_string());

        let _ = std::fs::create_dir_all(&dbpath_str);

        let imgcache_dir = path.path(DataPathType::Cache).join(ImageCache::rel_dir());
        let _ = std::fs::create_dir_all(imgcache_dir.clone());

        let mapsize = if cfg!(target_os = "windows") {
            // 16 Gib on windows because it actually creates the file
            1024usize * 1024usize * 1024usize * 16usize
        } else {
            // 1 TiB for everything else since its just virtually mapped
            1024usize * 1024usize * 1024usize * 1024usize
        };

        let theme = ThemeHandler::new(&path);
        ctx.options_mut(|o| {
            let cur_theme = theme.load();
            info!("Loaded theme {:?} from disk", cur_theme);
            o.theme_preference = cur_theme;
        });
        ctx.set_visuals_of(
            egui::Theme::Dark,
            theme::dark_mode(notedeck::ui::is_compiled_as_mobile()),
        );
        ctx.set_visuals_of(egui::Theme::Light, theme::light_mode());

        let config = Config::new().set_ingester_threads(4).set_mapsize(mapsize);

        let keystore = if parsed_args.use_keystore {
            let keys_path = path.path(DataPathType::Keys);
            let selected_key_path = path.path(DataPathType::SelectedKey);
            KeyStorageType::FileSystem(FileKeyStorage::new(
                Directory::new(keys_path),
                Directory::new(selected_key_path),
            ))
        } else {
            KeyStorageType::None
        };

        let mut accounts = Accounts::new(keystore, parsed_args.relays);

        let num_keys = parsed_args.keys.len();

        let mut unknown_ids = UnknownIds::default();
        let ndb = Ndb::new(&dbpath_str, &config).expect("ndb");

        {
            let txn = Transaction::new(&ndb).expect("txn");
            for key in parsed_args.keys {
                info!("adding account: {}", key.pubkey);
                accounts
                    .add_account(key)
                    .process_action(&mut unknown_ids, &ndb, &txn);
            }
        }

        if num_keys != 0 {
            accounts.select_account(0);
        }

        // AccountManager will setup the pool on first update
        let pool = RelayPool::new();

        let img_cache = ImageCache::new(imgcache_dir);
        let note_cache = NoteCache::default();
        let unknown_ids = UnknownIds::default();
        let egui = ctx.clone();
        let tabs = Tabs::new(None);
        let parsed_args = Args::parse(args);
        let app_rect_handler = AppSizeHandler::new(&path);

        Self {
            ndb,
            img_cache,
            app_rect_handler,
            unknown_ids,
            pool,
            note_cache,
            accounts,
            path: path.clone(),
            args: parsed_args,
            theme,
            egui,
            tabs,
        }
    }

    pub fn app_context(&mut self) -> AppContext<'_> {
        AppContext {
            ndb: &self.ndb,
            img_cache: &mut self.img_cache,
            unknown_ids: &mut self.unknown_ids,
            pool: &mut self.pool,
            note_cache: &mut self.note_cache,
            accounts: &mut self.accounts,
            path: &self.path,
            args: &self.args,
            theme: &mut self.theme,
            egui: &self.egui,
        }
    }

    pub fn add_app<T: notedeck::App + 'static>(&mut self, app: T) {
        self.tabs.app = Some(Rc::new(RefCell::new(app)));
    }
}

// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;

fn setup_logging(path: &DataPath) {
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
}

// Desktop
#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    let base_path = DataPath::default_base().unwrap_or(PathBuf::from_str(".").unwrap());
    let path = DataPath::new(&base_path);

    setup_logging(&path);

    let _res = eframe::run_native(
        "Damus Notedeck",
        generate_native_options(path),
        Box::new(|cc| {
            let args: Vec<String> = std::env::args().collect();
            let mut notedeck = Notedeck::new(&cc.egui_ctx, base_path, &args);

            let damus = Damus::new(&mut notedeck.app_context(), &args);
            notedeck.add_app(damus);

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
        let args: Vec<String> = vec![
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
        let args: Vec<String> = vec![
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
        let mut app_ctx = notedeck.app_context();
        let app = Damus::new(&mut app_ctx, &args);

        assert_eq!(app.columns(app_ctx.accounts).columns().len(), 2);

        let tl1 = app
            .columns(app_ctx.accounts)
            .column(0)
            .router()
            .top()
            .timeline_id();

        let tl2 = app
            .columns(app_ctx.accounts)
            .column(1)
            .router()
            .top()
            .timeline_id();

        assert_eq!(tl1.is_some(), true);
        assert_eq!(tl2.is_some(), true);

        let timelines = app.columns(app_ctx.accounts).timelines();
        assert!(timelines[0].kind.is_notifications());
        assert!(timelines[1].kind.is_contacts());

        rmrf(tmpdir);
    }
}
