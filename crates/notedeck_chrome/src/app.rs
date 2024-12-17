use crate::{app_size::AppSizeHandler, setup::setup_cc, theme};

use notedeck::{
    Accounts, AppContext, Args, DataPath, DataPathType, Directory, FileKeyStorage, ImageCache,
    KeyStorageType, NoteCache, ThemeHandler, UnknownIds,
};

use enostr::RelayPool;
use nostrdb::{Config, Ndb, Transaction};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use tracing::info;

/// Our browser app state
pub struct Notedeck {
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
        if !parsed_args.tests && cfg!(debug_assertions) && !parsed_args.debug {
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
            ndb: &mut self.ndb,
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

struct Tabs {
    app: Option<Rc<RefCell<dyn notedeck::App>>>,
}

impl Tabs {
    pub fn new(app: Option<Rc<RefCell<dyn notedeck::App>>>) -> Self {
        Self { app }
    }
}
