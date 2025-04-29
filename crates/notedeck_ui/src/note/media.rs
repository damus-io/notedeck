use egui::{Button, Color32, Image, Response, Sense, Window};
use notedeck::{Images, MediaCacheType};

use crate::{
    gif::{handle_repaint, retrieve_latest_texture},
    images::{render_images, ImageType},
};

pub(crate) fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    images: Vec<(String, MediaCacheType)>,
    carousel_id: egui::Id,
) {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_size().x;
    let spinsz = if height > width { width } else { height };

    let show_popup = ui.ctx().memory(|mem| {
        mem.data
            .get_temp(carousel_id.with("show_popup"))
            .unwrap_or(false)
    });

    let current_image = show_popup.then(|| {
        ui.ctx().memory(|mem| {
            mem.data
                .get_temp::<(String, MediaCacheType)>(carousel_id.with("current_image"))
                .unwrap_or_else(|| (images[0].0.clone(), images[0].1))
        })
    });

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (image, cache_type) in images {
                        render_images(
                            ui,
                            img_cache,
                            &image,
                            ImageType::Content,
                            cache_type,
                            |ui| {
                                ui.allocate_space(egui::vec2(spinsz, spinsz));
                            },
                            |ui, _| {
                                ui.allocate_space(egui::vec2(spinsz, spinsz));
                            },
                            |ui, url, renderable_media, gifs| {
                                let texture = handle_repaint(
                                    ui,
                                    retrieve_latest_texture(&image, gifs, renderable_media),
                                );
                                let img_resp = ui.add(
                                    Button::image(
                                        Image::new(texture)
                                            .max_height(height)
                                            .corner_radius(5.0)
                                            .fit_to_original_size(1.0),
                                    )
                                    .frame(false),
                                );

                                if img_resp.clicked() {
                                    ui.ctx().memory_mut(|mem| {
                                        mem.data.insert_temp(carousel_id.with("show_popup"), true);
                                        mem.data.insert_temp(
                                            carousel_id.with("current_image"),
                                            (image.clone(), cache_type),
                                        );
                                    });
                                }

                                copy_link(url, img_resp);
                            },
                        );
                    }
                })
                .response
            })
            .inner
    });

    if show_popup {
        let current_image = current_image
            .as_ref()
            .expect("the image was actually clicked");
        let image = current_image.clone().0;
        let cache_type = current_image.clone().1;

        Window::new("image_popup")
            .title_bar(false)
            .fixed_size(ui.ctx().screen_rect().size())
            .fixed_pos(ui.ctx().screen_rect().min)
            .frame(egui::Frame::NONE)
            .show(ui.ctx(), |ui| {
                let screen_rect = ui.ctx().screen_rect();

                // escape
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    ui.ctx().memory_mut(|mem| {
                        mem.data.insert_temp(carousel_id.with("show_popup"), false);
                    });
                }

                // background
                ui.painter()
                    .rect_filled(screen_rect, 0.0, Color32::from_black_alpha(230));

                // zoom init
                let zoom_id = carousel_id.with("zoom_level");
                let mut zoom = ui
                    .ctx()
                    .memory(|mem| mem.data.get_temp(zoom_id).unwrap_or(1.0_f32));

                // pan init
                let pan_id = carousel_id.with("pan_offset");
                let mut pan_offset = ui
                    .ctx()
                    .memory(|mem| mem.data.get_temp(pan_id).unwrap_or(egui::Vec2::ZERO));

                // zoom & scroll
                if ui.input(|i| i.pointer.hover_pos()).is_some() {
                    let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
                    if scroll_delta.y != 0.0 {
                        let zoom_factor = if scroll_delta.y > 0.0 { 1.05 } else { 0.95 };
                        zoom *= zoom_factor;
                        zoom = zoom.clamp(0.1, 5.0);

                        if zoom <= 1.0 {
                            pan_offset = egui::Vec2::ZERO;
                        }

                        ui.ctx().memory_mut(|mem| {
                            mem.data.insert_temp(zoom_id, zoom);
                            mem.data.insert_temp(pan_id, pan_offset);
                        });
                    }
                }

                ui.centered_and_justified(|ui| {
                    render_images(
                        ui,
                        img_cache,
                        &image,
                        ImageType::Content,
                        cache_type,
                        |ui| {
                            ui.allocate_space(egui::vec2(spinsz, spinsz));
                        },
                        |ui, _| {
                            ui.allocate_space(egui::vec2(spinsz, spinsz));
                        },
                        |ui, url, renderable_media, gifs| {
                            let texture = handle_repaint(
                                ui,
                                retrieve_latest_texture(&image, gifs, renderable_media),
                            );

                            let texture_size = texture.size_vec2();
                            let screen_size = screen_rect.size();
                            let scale = (screen_size.x / texture_size.x)
                                .min(screen_size.y / texture_size.y)
                                .min(1.0);
                            let scaled_size = texture_size * scale * zoom;

                            let visible_width = scaled_size.x.min(screen_size.x);
                            let visible_height = scaled_size.y.min(screen_size.y);

                            let max_pan_x = ((scaled_size.x - visible_width) / 2.0).max(0.0);
                            let max_pan_y = ((scaled_size.y - visible_height) / 2.0).max(0.0);

                            if max_pan_x > 0.0 {
                                pan_offset.x = pan_offset.x.clamp(-max_pan_x, max_pan_x);
                            } else {
                                pan_offset.x = 0.0;
                            }

                            if max_pan_y > 0.0 {
                                pan_offset.y = pan_offset.y.clamp(-max_pan_y, max_pan_y);
                            } else {
                                pan_offset.y = 0.0;
                            }

                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(visible_width, visible_height),
                                egui::Sense::click_and_drag(),
                            );

                            let uv_min = egui::pos2(
                                0.5 - (visible_width / scaled_size.x) / 2.0
                                    + pan_offset.x / scaled_size.x,
                                0.5 - (visible_height / scaled_size.y) / 2.0
                                    + pan_offset.y / scaled_size.y,
                            );

                            let uv_max = egui::pos2(
                                uv_min.x + visible_width / scaled_size.x,
                                uv_min.y + visible_height / scaled_size.y,
                            );

                            let uv = egui::Rect::from_min_max(uv_min, uv_max);

                            ui.painter()
                                .image(texture.id(), rect, uv, egui::Color32::WHITE);
                            let img_rect = ui.allocate_rect(rect, Sense::click());

                            if img_rect.clicked() {
                                ui.ctx().memory_mut(|mem| {
                                    mem.data.insert_temp(carousel_id.with("show_popup"), true);
                                });
                            } else if img_rect.clicked_elsewhere() {
                                ui.ctx().memory_mut(|mem| {
                                    mem.data.insert_temp(carousel_id.with("show_popup"), false);
                                });
                            }

                            // Handle dragging for pan
                            if response.dragged() {
                                let delta = response.drag_delta();

                                pan_offset.x -= delta.x;
                                pan_offset.y -= delta.y;

                                if max_pan_x > 0.0 {
                                    pan_offset.x = pan_offset.x.clamp(-max_pan_x, max_pan_x);
                                } else {
                                    pan_offset.x = 0.0;
                                }

                                if max_pan_y > 0.0 {
                                    pan_offset.y = pan_offset.y.clamp(-max_pan_y, max_pan_y);
                                } else {
                                    pan_offset.y = 0.0;
                                }

                                ui.ctx().memory_mut(|mem| {
                                    mem.data.insert_temp(pan_id, pan_offset);
                                });
                            }

                            // reset zoom on double-click
                            if response.double_clicked() {
                                pan_offset = egui::Vec2::ZERO;
                                zoom = 1.0;
                                ui.ctx().memory_mut(|mem| {
                                    mem.data.insert_temp(pan_id, pan_offset);
                                    mem.data.insert_temp(zoom_id, zoom);
                                });
                            }

                            copy_link(url, response);
                        },
                    );
                });
            });
    }
}

fn copy_link(url: &str, img_resp: Response) {
    img_resp.context_menu(|ui| {
        if ui.button("Copy Link").clicked() {
            ui.ctx().copy_text(url.to_owned());
            ui.close_menu();
        }
    });
}
