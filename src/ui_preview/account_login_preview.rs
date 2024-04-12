use crate::egui_preview_setup::{EguiPreviewCase, EguiPreviewSetup};
use notedeck::account_login_view::{DesktopAccountLoginView, MobileAccountLoginView};
use notedeck::login_manager::LoginManager;

pub struct DesktopAccountLoginPreview {
    manager: LoginManager,
}

impl EguiPreviewCase for DesktopAccountLoginPreview {
    fn new(_supr: EguiPreviewSetup) -> Self {
        DesktopAccountLoginPreview {
            manager: LoginManager::new(),
        }
    }
}

impl eframe::App for DesktopAccountLoginPreview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        DesktopAccountLoginView::new(ctx, &mut self.manager).panel()
    }
}

pub struct MobileAccountLoginPreview {
    manager: LoginManager,
}

impl EguiPreviewCase for MobileAccountLoginPreview {
    fn new(_supr: EguiPreviewSetup) -> Self {
        MobileAccountLoginPreview {
            manager: LoginManager::new(),
        }
    }
}

impl eframe::App for MobileAccountLoginPreview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        MobileAccountLoginView::new(ctx, &mut self.manager).panel()
    }
}