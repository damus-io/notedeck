use crate::egui_test_setup::{EguiTestCase, EguiTestSetup};
use notedeck::account_login_view::AccountLoginView;
use notedeck::login_manager::LoginManager;

pub struct AccountLoginTest {
    manager: LoginManager,
}

impl EguiTestCase for AccountLoginTest {
    fn new(_supr: EguiTestSetup) -> Self {
        AccountLoginTest {
            manager: LoginManager::new(),
        }
    }
}

impl eframe::App for AccountLoginTest {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        AccountLoginView::new(ctx, &mut self.manager).panel()
    }
}
