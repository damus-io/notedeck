mod account_login_view_test;
mod egui_test_setup;
use account_login_view_test::AccountLoginTest;
use egui_test_setup::{EguiTestCase, EguiTestSetup};
use notedeck::app_creation::generate_native_options;
use std::env;

fn run_test_app<F, T, O>(create_supr: F, create_child: O)
where
    F: 'static + FnOnce(&eframe::CreationContext<'_>) -> EguiTestSetup,
    T: 'static + EguiTestCase,
    O: 'static + FnOnce(EguiTestSetup) -> T,
{
    tracing_subscriber::fmt::init();

    let _ = eframe::run_native(
        "UI Test Harness",
        generate_native_options(),
        Box::new(|cc| Box::new(create_child(create_supr(cc)))),
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "AccountLoginView" => run_test_app(EguiTestSetup::new, AccountLoginTest::new),
            _ => println!("Component not found."),
        }
    } else {
        println!("Please specify a component to test.");
    }
}
