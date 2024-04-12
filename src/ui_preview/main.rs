mod account_login_preview;
mod egui_preview_setup;
use account_login_preview::{DesktopAccountLoginPreview, MobileAccountLoginPreview};
use egui_preview_setup::{EguiPreviewCase, EguiPreviewSetup};
use notedeck::app_creation::{generate_native_options, generate_mobile_emulator_native_options};
use std::env;

fn run_test_app<F, T, O>(create_supr: F, create_child: O, is_mobile: bool)
where
    F: 'static + FnOnce(&eframe::CreationContext<'_>) -> EguiPreviewSetup,
    T: 'static + EguiPreviewCase,
    O: 'static + FnOnce(EguiPreviewSetup) -> T,
{
    tracing_subscriber::fmt::init();

    let native_options = if is_mobile {
        generate_mobile_emulator_native_options()
    } else {
        generate_native_options()
    };

    let _ = eframe::run_native(
        "UI Preview Runner",
        native_options,
        Box::new(|cc| Box::new(create_child(create_supr(cc)))),
    );
}


fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "DesktopAccountLoginPreview" => {
                run_test_app(EguiPreviewSetup::new, DesktopAccountLoginPreview::new, false)
            }
            "MobileAccountLoginPreview" => {
                run_test_app(EguiPreviewSetup::new, MobileAccountLoginPreview::new, true)
            }
            _ => println!("Component not found."),
        }
    } else {
        println!("Please specify a component to test.");
    }
}
