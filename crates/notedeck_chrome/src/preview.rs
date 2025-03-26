use notedeck::{DataPath, Notedeck};
use notedeck_chrome::setup::{generate_native_options, setup_chrome};
use notedeck_columns::ui::configure_deck::ConfigureDeckView;
use notedeck_columns::ui::edit_deck::EditDeckView;
use notedeck_columns::ui::profile::EditProfileView;
use notedeck_columns::ui::{
    account_login_view::AccountLoginView, PostView, Preview, PreviewApp, PreviewConfig,
    ProfilePreview, RelayView,
};
use std::env;

struct PreviewRunner {}

impl PreviewRunner {
    fn new() -> Self {
        PreviewRunner {}
    }

    async fn run<P>(self, preview: P)
    where
        P: notedeck::App + 'static,
    {
        tracing_subscriber::fmt::init();

        let base_path = DataPath::default_base_or_cwd();
        let path = DataPath::new(&base_path);

        let _res = eframe::run_native(
            "Notedeck Preview",
            generate_native_options(path),
            Box::new(|cc| {
                let args: Vec<String> = std::env::args().collect();
                let ctx = &cc.egui_ctx;

                let mut notedeck = Notedeck::new(ctx, &base_path, &args);
                assert!(
                    notedeck.unrecognized_args().is_empty(),
                    "unrecognized args: {:?}",
                    notedeck.unrecognized_args()
                );
                setup_chrome(ctx, notedeck.args(), notedeck.theme());

                notedeck.set_app(PreviewApp::new(preview));

                Ok(Box::new(notedeck))
            }),
        );
    }
}

macro_rules! previews {
    // Accept a runner and name variable, followed by one or more identifiers for the views
    ($runner:expr, $name:expr, $is_mobile:expr, $($view:ident),* $(,)?) => {
        match $name.as_ref() {
            $(
                stringify!($view) => {
                    $runner.run($view::preview(PreviewConfig { is_mobile: $is_mobile })).await;
                }
            )*
            _ => println!("Component not found."),
        }
    };
}

#[tokio::main]
async fn main() {
    let mut name: Option<String> = None;
    let mut is_mobile: Option<bool> = None;
    let mut light_mode: bool = false;

    for arg in env::args() {
        if arg == "--mobile" {
            is_mobile = Some(true);
        } else if arg == "--light" {
            light_mode = true;
        } else {
            name = Some(arg);
        }
    }

    let name = if let Some(name) = name {
        name
    } else {
        println!("Please specify a component to test");
        return;
    };

    println!(
        "light mode previews: {}",
        if light_mode { "enabled" } else { "disabled" }
    );
    let is_mobile = is_mobile.unwrap_or(notedeck::ui::is_compiled_as_mobile());
    let runner = PreviewRunner::new();

    previews!(
        runner,
        name,
        is_mobile,
        RelayView,
        AccountLoginView,
        ProfilePreview,
        PostView,
        ConfigureDeckView,
        EditDeckView,
        EditProfileView,
    );
}
