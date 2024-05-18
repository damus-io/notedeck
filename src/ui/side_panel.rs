use egui::{Button, Layout, SidePanel, Vec2};

use crate::ui::global_popup::GlobalPopupType;

use super::{
    persist_state::{PERSISTED_GLOBAL_POPUP, PERSISTED_SIDE_PANEL},
    View,
};

#[derive(Default)]
pub struct DesktopSidePanel {}

static ID: &str = "left panel";

impl View for DesktopSidePanel {
    fn ui(&mut self, ui: &mut egui::Ui) {
        DesktopSidePanel::inner(ui);
    }
}

impl DesktopSidePanel {
    pub fn new() -> Self {
        DesktopSidePanel::default()
    }

    pub fn inner(ui: &mut egui::Ui) {
        let dark_mode = ui.ctx().style().visuals.dark_mode;
        let spacing_amt = 16.0;
        ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(spacing_amt);
            if ui
                .add_sized(Vec2::new(32.0, 32.0), Button::new("A"))
                .clicked()
            {
                PERSISTED_SIDE_PANEL.set_state(ui.ctx(), Some(GlobalPopupType::AccountManagement));
                PERSISTED_GLOBAL_POPUP.set_state(ui.ctx(), true);
            }
            ui.add_space(spacing_amt);
            ui.add(settings_button(dark_mode));
            ui.add_space(spacing_amt);
            ui.add(add_column_button(dark_mode));
            ui.add_space(spacing_amt);
        });
    }

    pub fn panel() -> SidePanel {
        egui::SidePanel::left(ID).resizable(false).exact_width(40.0)
    }
}

fn settings_button(dark_mode: bool) -> egui::Button<'static> {
    let _ = dark_mode;
    let img_data = egui::include_image!("../../assets/icons/settings_dark_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(32.0)).frame(false)
}

fn add_column_button(dark_mode: bool) -> egui::Button<'static> {
    let _ = dark_mode;
    let img_data = egui::include_image!("../../assets/icons/add_column_dark_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(32.0)).frame(false)
}

mod preview {
    use crate::ui::Preview;

    use super::*;

    pub struct DesktopSidePanelPreview {}

    impl DesktopSidePanelPreview {
        fn new() -> Self {
            DesktopSidePanelPreview {}
        }
    }

    impl View for DesktopSidePanelPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut panel = DesktopSidePanel::new();
            DesktopSidePanel::panel().show(ui.ctx(), |ui| panel.ui(ui));
        }
    }

    impl Preview for DesktopSidePanel {
        type Prev = DesktopSidePanelPreview;

        fn preview() -> Self::Prev {
            DesktopSidePanelPreview::new()
        }
    }
}
