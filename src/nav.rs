use crate::{
    account_manager::render_accounts_route,
    app_style::{get_font_size, NotedeckTextStyle},
    fonts::NamedFontFamily,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    thread::thread_unsubscribe,
    timeline::route::{render_timeline_route, AfterRouteExecution, TimelineRoute},
    ui::{
        self,
        add_column::{AddColumnResponse, AddColumnView},
        anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
        note::PostAction,
        RelayView, View,
    },
    Damus,
};

use egui::{pos2, Color32, InnerResponse};
use egui_nav::{Nav, NavAction};

pub fn render_nav(col: usize, app: &mut Damus, ui: &mut egui::Ui) {
    let col_id = app.columns.get_column_id_at_index(col);
    // TODO(jb55): clean up this router_mut mess by using Router<R> in egui-nav directly
    let routes = app
        .columns()
        .column(col)
        .router()
        .routes()
        .iter()
        .map(|r| r.get_titled_route(&app.columns, &app.ndb))
        .collect();
    let nav_response = Nav::new(routes)
        .navigating(app.columns_mut().column_mut(col).router_mut().navigating)
        .returning(app.columns_mut().column_mut(col).router_mut().returning)
        .title(48.0, title_bar)
        .show_mut(col_id, ui, |ui, nav| {
            let column = app.columns.column_mut(col);
            match &nav.top().route {
                Route::Timeline(tlr) => render_timeline_route(
                    &app.ndb,
                    &mut app.columns,
                    &mut app.pool,
                    &mut app.drafts,
                    &mut app.img_cache,
                    &mut app.note_cache,
                    &mut app.threads,
                    &mut app.accounts,
                    *tlr,
                    col,
                    app.textmode,
                    ui,
                ),
                Route::Accounts(amr) => {
                    render_accounts_route(
                        ui,
                        &app.ndb,
                        col,
                        &mut app.columns,
                        &mut app.img_cache,
                        &mut app.accounts,
                        &mut app.view_state.login,
                        *amr,
                    );
                    None
                }
                Route::Relays => {
                    let manager = RelayPoolManager::new(app.pool_mut());
                    RelayView::new(manager).ui(ui);
                    None
                }
                Route::ComposeNote => {
                    let kp = app.accounts.selected_or_first_nsec()?;
                    let draft = app.drafts.compose_mut();

                    let txn = nostrdb::Transaction::new(&app.ndb).expect("txn");
                    let post_response = ui::PostView::new(
                        &app.ndb,
                        draft,
                        crate::draft::DraftSource::Compose,
                        &mut app.img_cache,
                        &mut app.note_cache,
                        kp,
                    )
                    .ui(&txn, ui);

                    if let Some(action) = post_response.action {
                        PostAction::execute(kp, &action, &mut app.pool, draft, |np, seckey| {
                            np.to_note(seckey)
                        });
                        column.router_mut().go_back();
                    }

                    None
                }
                Route::AddColumn => {
                    let resp =
                        AddColumnView::new(&app.ndb, app.accounts.get_selected_account()).ui(ui);

                    if let Some(resp) = resp {
                        match resp {
                            AddColumnResponse::Timeline(timeline) => {
                                let id = timeline.id;
                                app.columns_mut().add_timeline_to_column(col, timeline);
                                app.subscribe_new_timeline(id);
                            }
                        };
                    }
                    None
                }
            }
        });

    if let Some(after_route_execution) = nav_response.inner {
        // start returning when we're finished posting
        match after_route_execution {
            AfterRouteExecution::Post(resp) => {
                if let Some(action) = resp.action {
                    match action {
                        PostAction::Post(_) => {
                            app.columns_mut().column_mut(col).router_mut().returning = true;
                        }
                    }
                }
            }
        }
    }

    if let Some(NavAction::Returned) = nav_response.action {
        let r = app.columns_mut().column_mut(col).router_mut().pop();
        if let Some(Route::Timeline(TimelineRoute::Thread(id))) = r {
            thread_unsubscribe(
                &app.ndb,
                &mut app.threads,
                &mut app.pool,
                &mut app.note_cache,
                id.bytes(),
            );
        }
    } else if let Some(NavAction::Navigated) = nav_response.action {
        let cur_router = app.columns_mut().column_mut(col).router_mut();
        cur_router.navigating = false;
        if cur_router.is_replacing() {
            cur_router.remove_previous_route();
        }
    }

    if let Some(title_response) = nav_response.title_response {
        match title_response {
            TitleResponse::RemoveColumn => {
                app.columns_mut().request_deletion_at_index(col);
            }
        }
    }
}

fn title_bar(
    ui: &mut egui::Ui,
    title_name: String,
    allocated_response: egui::Response,
) -> egui::InnerResponse<Option<TitleResponse>> {
    let icon_width = 32.0;
    let padding = 16.0;
    title(ui, title_name, allocated_response.rect, icon_width, padding);
    let button_resp = delete_column_button(ui, allocated_response, icon_width, padding);
    let title_response = if button_resp.clicked() {
        Some(TitleResponse::RemoveColumn)
    } else {
        None
    };

    InnerResponse::new(title_response, button_resp)
}

fn delete_column_button(
    ui: &mut egui::Ui,
    title_bar_resp: egui::Response,
    icon_width: f32,
    padding: f32,
) -> egui::Response {
    let img_size = 16.0;
    let max_size = icon_width * ICON_EXPANSION_MULTIPLE;

    let img_data = egui::include_image!("../assets/icons/column_delete_icon_4x.png");
    let img = egui::Image::new(img_data).max_width(img_size);

    let button_rect = {
        let titlebar_rect = title_bar_resp.rect;
        let titlebar_width = titlebar_rect.width();
        let titlebar_center = titlebar_rect.center();
        let button_center_y = titlebar_center.y;
        let button_center_x =
            titlebar_center.x + (titlebar_width / 2.0) - (max_size / 2.0) - padding;
        egui::Rect::from_center_size(
            pos2(button_center_x, button_center_y),
            egui::vec2(max_size, max_size),
        )
    };

    let helper = AnimationHelper::new_from_rect(ui, "delete-column-button", button_rect);

    let cur_img_size = helper.scale_1d_pos(img_size);

    let animation_rect = helper.get_animation_rect();
    let animation_resp = helper.take_animation_response();
    if title_bar_resp.union(animation_resp.clone()).hovered() {
        img.paint_at(ui, animation_rect.shrink((max_size - cur_img_size) / 2.0));
    }

    animation_resp
}

fn title(
    ui: &mut egui::Ui,
    title_name: String,
    titlebar_rect: egui::Rect,
    icon_width: f32,
    padding: f32,
) {
    let painter = ui.painter_at(titlebar_rect);

    let font = egui::FontId::new(
        get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
        egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
    );

    let max_title_width = titlebar_rect.width() - icon_width - padding * 2.;
    let title_galley =
        ui.fonts(|f| f.layout(title_name, font, ui.visuals().text_color(), max_title_width));

    let pos = {
        let titlebar_center = titlebar_rect.center();
        let titlebar_width = titlebar_rect.width();
        let text_height = title_galley.rect.height();

        let galley_pos_x = titlebar_center.x - (titlebar_width / 2.) + padding;
        let galley_pos_y = titlebar_center.y - (text_height / 2.);
        pos2(galley_pos_x, galley_pos_y)
    };

    painter.galley(pos, title_galley, Color32::WHITE);
}

enum TitleResponse {
    RemoveColumn,
}
