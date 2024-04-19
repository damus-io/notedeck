use notedeck::account_login_view::AccountLoginView;
use notedeck::app_creation::{
    generate_mobile_emulator_native_options, generate_native_options, setup_cc,
};
use notedeck::relay_view::RelayView;
use notedeck::ui::{Preview, PreviewApp};
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

#[tokio::main]
async fn main() {
    let mut name: Option<String> = None;
    let mut is_mobile = false;

    for arg in env::args() {
        if arg == "--mobile" {
            is_mobile = true;
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

    let runner = PreviewRunner::new(is_mobile);

    match name.as_ref() {
        "AccountLoginView" => {
            runner.run(AccountLoginView::preview()).await;
        }
        "RelayView" => {
            runner.run(RelayView::preview()).await;
        }
        _ => println!("Component not found."),
    }
}
