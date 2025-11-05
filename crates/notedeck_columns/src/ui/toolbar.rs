use egui::{Color32, Layout};
use notedeck_ui::icons::{home_button, notifications_button};

use crate::{toolbar::ToolbarAction, ui::side_panel::search_button_impl, Damus};

#[profiling::function]
pub fn toolbar(ui: &mut egui::Ui, unseen_notification: bool) -> Option<ToolbarAction> {
    use egui_tabs::{TabColor, Tabs};

    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );

    if !ui.visuals().dark_mode {
        ui.painter().rect(
            rect,
            0,
            notedeck_ui::colors::ALMOST_WHITE,
            egui::Stroke::new(0.0, Color32::TRANSPARENT),
            egui::StrokeKind::Inside,
        );
    }

    let rs = Tabs::new(3)
        .selected(Damus::initially_selected_toolbar_index())
        .hover_bg(TabColor::none())
        .selected_fg(TabColor::none())
        .selected_bg(TabColor::none())
        .height(Damus::toolbar_height())
        .layout(Layout::centered_and_justified(egui::Direction::TopDown))
        .show(ui, |ui, state| {
            let index = state.index();

            let mut action: Option<ToolbarAction> = None;

            let btn_size: f32 = 20.0;
            if index == 0 {
                if home_button(ui, btn_size).clicked() {
                    action = Some(ToolbarAction::Home);
                }
            } else if index == 1
                && ui
                    .add(search_button_impl(ui.visuals().text_color(), 2.0, false))
                    .clicked()
            {
                action = Some(ToolbarAction::Search)
            } else if index == 2
                && notifications_button(ui, btn_size, unseen_notification).clicked()
            {
                action = Some(ToolbarAction::Notifications);
            }

            action
        })
        .inner();

    for maybe_r in rs {
        if maybe_r.inner.is_some() {
            return maybe_r.inner;
        }
    }

    None
}
