use notedeck::app_creation::{
    generate_mobile_emulator_native_options, generate_native_options, setup_cc,
};
use notedeck::ui::account_login_view::AccountLoginView;
use notedeck::ui::{
    AccountManagementView, AccountSelectionWidget, DesktopSidePanel, Preview, PreviewApp,
    PreviewConfig, ProfilePic, ProfilePreview, RelayView,
};
use std::env;

struct PreviewRunner {
    force_mobile: bool,
}

impl PreviewRunner {
    fn new(force_mobile: bool) -> Self {
        PreviewRunner { force_mobile }
    }

    async fn run<P>(self, preview: P)
    where
        P: Into<PreviewApp> + 'static,
    {
        tracing_subscriber::fmt::init();

        let native_options = if self.force_mobile {
            generate_mobile_emulator_native_options()
        } else {
            generate_native_options()
        };

        let is_mobile = self.force_mobile;
        let _ = eframe::run_native(
            "UI Preview Runner",
            native_options,
            Box::new(move |cc| {
                setup_cc(cc, is_mobile);
                Box::new(Into::<PreviewApp>::into(preview))
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

    for arg in env::args() {
        if arg == "--mobile" {
            is_mobile = Some(true);
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

    let is_mobile = is_mobile.unwrap_or(notedeck::ui::is_compiled_as_mobile());
    let runner = PreviewRunner::new(is_mobile);

    previews!(
        runner,
        name,
        is_mobile,
        RelayView,
        AccountLoginView,
        ProfilePreview,
        ProfilePic,
        AccountManagementView,
        AccountSelectionWidget,
        DesktopSidePanel,
    );
}
