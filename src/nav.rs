use crate::{
    account_manager::render_accounts_route,
    app::{get_active_columns, get_active_columns_mut},
    app_style::{get_font_size, NotedeckTextStyle},
    fonts::NamedFontFamily,
    notes_holder::NotesHolder,
    profile::Profile,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    thread::Thread,
    timeline::{
        route::{render_profile_route, render_timeline_route, AfterRouteExecution, TimelineRoute},
        Timeline,
    },
    ui::{
        self,
        add_column::{AddColumnResponse, AddColumnView},
        anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
        note::PostAction,
        support::SupportView,
        RelayView, View,
    },
    Damus,
};

use egui::{pos2, Color32, InnerResponse, Stroke};
use egui_nav::{Nav, NavAction, TitleBarResponse};
use nostrdb::{Ndb, Transaction};
use tracing::{error, info};

pub fn render_nav(col: usize, app: &mut Damus, ui: &mut egui::Ui) {
    let col_id = app.columns().get_column_id_at_index(col);
    // TODO(jb55): clean up this router_mut mess by using Router<R> in egui-nav directly
    let routes = get_active_columns(&app.accounts, &app.decks_cache)
        .column(col)
        .router()
        .routes()
        .iter()
        .map(|r| {
            r.get_titled_route(
                get_active_columns(&app.accounts, &app.decks_cache),
                &app.ndb,
            )
        })
        .collect();
    let nav_response = Nav::new(routes)
        .navigating(app.columns_mut().column_mut(col).router_mut().navigating)
        .returning(app.columns_mut().column_mut(col).router_mut().returning)
        .title(48.0, title_bar)
        .show_mut(col_id, ui, |ui, nav| {
            let column =
                get_active_columns_mut(&app.accounts, &mut app.decks_cache).column_mut(col);
            match &nav.top().route {
                Route::Timeline(tlr) => render_timeline_route(
                    &app.ndb,
                    get_active_columns_mut(&app.accounts, &mut app.decks_cache),
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
                        &mut app.img_cache,
                        &mut app.accounts,
                        &mut app.decks_cache,
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

                Route::Profile(pubkey) => render_profile_route(
                    pubkey,
                    &app.ndb,
                    get_active_columns_mut(&app.accounts, &mut app.decks_cache),
                    &mut app.profiles,
                    &mut app.pool,
                    &mut app.img_cache,
                    &mut app.note_cache,
                    &mut app.threads,
                    col,
                    ui,
                ),
                Route::Support => {
                    SupportView::new(&mut app.support).show(ui);
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

            AfterRouteExecution::OpenProfile(pubkey) => {
                get_active_columns_mut(&app.accounts, &mut app.decks_cache)
                    .column_mut(col)
                    .router_mut()
                    .route_to(Route::Profile(pubkey));
                let txn = Transaction::new(&app.ndb).expect("txn");
                if let Some(res) = Profile::open(
                    &app.ndb,
                    &mut app.note_cache,
                    &txn,
                    &mut app.pool,
                    &mut app.profiles,
                    pubkey.bytes(),
                ) {
                    res.process(&app.ndb, &mut app.note_cache, &txn, &mut app.profiles);
                }
            }
        }
    }

    if let Some(NavAction::Returned) = nav_response.action {
        let r = app.columns_mut().column_mut(col).router_mut().pop();
        let txn = Transaction::new(&app.ndb).expect("txn");
        if let Some(Route::Timeline(TimelineRoute::Thread(id))) = r {
            let root_id = {
                crate::note::root_note_id_from_selected_id(
                    &app.ndb,
                    &mut app.note_cache,
                    &txn,
                    id.bytes(),
                )
            };
            Thread::unsubscribe_locally(
                &txn,
                &app.ndb,
                &mut app.note_cache,
                &mut app.threads,
                &mut app.pool,
                root_id,
            );
        }

        if let Some(Route::Profile(pubkey)) = r {
            Profile::unsubscribe_locally(
                &txn,
                &app.ndb,
                &mut app.note_cache,
                &mut app.profiles,
                &mut app.pool,
                pubkey.bytes(),
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
                let tl = app.columns().find_timeline_for_column_index(col);
                if let Some(timeline) = tl {
                    unsubscribe_timeline(app.ndb(), timeline);
                }
            }
        }
    }
}

fn unsubscribe_timeline(ndb: &Ndb, timeline: &Timeline) {
    if let Some(sub_id) = timeline.subscription {
        if let Err(e) = ndb.unsubscribe(sub_id) {
            error!("unsubscribe error: {}", e);
        } else {
            info!(
                "successfully unsubscribed from timeline {} with sub id {}",
                timeline.id,
                sub_id.id()
            );
        }
    }
}

fn title_bar(
    ui: &mut egui::Ui,
    allocated_response: egui::Response,
    title_name: String,
    back_name: Option<String>,
) -> egui::InnerResponse<TitleBarResponse<TitleResponse>> {
    let icon_width = 32.0;
    let padding_external = 16.0;
    let padding_internal = 8.0;
    let has_back = back_name.is_some();

    let (spacing_rect, titlebar_rect) = allocated_response
        .rect
        .split_left_right_at_x(allocated_response.rect.left() + padding_external);
    ui.advance_cursor_after_rect(spacing_rect);

    let (titlebar_resp, maybe_button_resp) = if has_back {
        let (button_rect, titlebar_rect) = titlebar_rect
            .split_left_right_at_x(allocated_response.rect.left() + icon_width + padding_external);
        (
            allocated_response.with_new_rect(titlebar_rect),
            Some(back_button(ui, button_rect)),
        )
    } else {
        (allocated_response, None)
    };

    title(
        ui,
        title_name,
        titlebar_resp.rect,
        icon_width,
        if has_back {
            padding_internal
        } else {
            padding_external
        },
    );

    let delete_button_resp = delete_column_button(ui, titlebar_resp, icon_width, padding_external);
    let title_response = if delete_button_resp.clicked() {
        Some(TitleResponse::RemoveColumn)
    } else {
        None
    };

    let titlebar_resp = TitleBarResponse {
        title_response,
        go_back: maybe_button_resp.map_or(false, |r| r.clicked()),
    };

    InnerResponse::new(titlebar_resp, delete_button_resp)
}

fn back_button(ui: &mut egui::Ui, button_rect: egui::Rect) -> egui::Response {
    let horizontal_length = 10.0;
    let arrow_length = 5.0;

    let helper = AnimationHelper::new_from_rect(ui, "note-compose-button", button_rect);
    let painter = ui.painter_at(helper.get_animation_rect());
    let stroke = Stroke::new(1.5, ui.visuals().text_color());

    // Horizontal segment
    let left_horizontal_point = pos2(-horizontal_length / 2., 0.);
    let right_horizontal_point = pos2(horizontal_length / 2., 0.);
    let scaled_left_horizontal_point = helper.scale_pos_from_center(left_horizontal_point);
    let scaled_right_horizontal_point = helper.scale_pos_from_center(right_horizontal_point);

    painter.line_segment(
        [scaled_left_horizontal_point, scaled_right_horizontal_point],
        stroke,
    );

    // Top Arrow
    let sqrt_2_over_2 = std::f32::consts::SQRT_2 / 2.;
    let right_top_arrow_point = helper.scale_pos_from_center(pos2(
        left_horizontal_point.x + (sqrt_2_over_2 * arrow_length),
        right_horizontal_point.y + sqrt_2_over_2 * arrow_length,
    ));

    let scaled_left_arrow_point = scaled_left_horizontal_point;
    painter.line_segment([scaled_left_arrow_point, right_top_arrow_point], stroke);

    let right_bottom_arrow_point = helper.scale_pos_from_center(pos2(
        left_horizontal_point.x + (sqrt_2_over_2 * arrow_length),
        right_horizontal_point.y - sqrt_2_over_2 * arrow_length,
    ));

    painter.line_segment([scaled_left_arrow_point, right_bottom_arrow_point], stroke);

    helper.take_animation_response()
}

fn delete_column_button(
    ui: &mut egui::Ui,
    allocation_response: egui::Response,
    icon_width: f32,
    padding: f32,
) -> egui::Response {
    let img_size = 16.0;
    let max_size = icon_width * ICON_EXPANSION_MULTIPLE;

    let img_data = egui::include_image!("../assets/icons/column_delete_icon_4x.png");
    let img = egui::Image::new(img_data).max_width(img_size);

    let button_rect = {
        let titlebar_rect = allocation_response.rect;
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
    if allocation_response.union(animation_resp.clone()).hovered() {
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
        let text_height = title_galley.rect.height();

        let galley_pos_x = titlebar_rect.left() + padding;
        let galley_pos_y = titlebar_center.y - (text_height / 2.);
        pos2(galley_pos_x, galley_pos_y)
    };

    painter.galley(pos, title_galley, Color32::WHITE);
}

enum TitleResponse {
    RemoveColumn,
}
