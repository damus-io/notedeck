use notedeck::app_creation::{
    generate_mobile_emulator_native_options, generate_native_options, setup_cc,
};
use notedeck::ui::account_login_view::AccountLoginView;
use notedeck::ui::{
    AccountManagementView, AccountSelectionWidget, DesktopSidePanel, Preview, PreviewApp,
    ProfilePic, ProfilePreview, RelayView,
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

        let _ = eframe::run_native(
            "UI Preview Runner",
            native_options,
            Box::new(|cc| {
                setup_cc(cc);
                Box::new(Into::<PreviewApp>::into(preview))
            }),
        );
    }
}

macro_rules! previews {
    // Accept a runner and name variable, followed by one or more identifiers for the views
    ($runner:expr, $name:expr, $($view:ident),* $(,)?) => {
        match $name.as_ref() {
            $(
                stringify!($view) => {
                    $runner.run($view::preview()).await;
                }
            )*
            _ => println!("Component not found."),
        }
    };
}

#[tokio::main]
async fn main() {
    let mut name: Option<String> = None;

    #[allow(unused_assignments)]
    #[allow(unused_mut)]
    let mut is_mobile = false;
    #[cfg(feature = "emulate_mobile")]
    {
        is_mobile = true
    }

    for arg in env::args() {
        name = Some(arg);
    }

    let name = if let Some(name) = name {
        name
    } else {
        println!("Please specify a component to test");
        return;
    };

    let runner = PreviewRunner::new(is_mobile);

    previews!(
        runner,
        name,
        RelayView,
        AccountLoginView,
        ProfilePreview,
        ProfilePic,
        AccountManagementView,
        AccountSelectionWidget,
        DesktopSidePanel,
    );
}
