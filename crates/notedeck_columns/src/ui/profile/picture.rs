use crate::images::ImageType;
use crate::ui::images::render_images;
use crate::ui::{Preview, PreviewConfig};
use egui::{vec2, Sense, Stroke, TextureHandle};
use nostrdb::{Ndb, Transaction};
use tracing::info;

use notedeck::{AppContext, Images};

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut Images,
    url: &'url str,
    size: f32,
    border: Option<Stroke>,
}

impl egui::Widget for ProfilePic<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_pfp(ui, self.cache, self.url, self.size, self.border)
    }
}

impl<'cache, 'url> ProfilePic<'cache, 'url> {
    pub fn new(cache: &'cache mut Images, url: &'url str) -> Self {
        let size = Self::default_size();
        ProfilePic {
            cache,
            url,
            size,
            border: None,
        }
    }

    pub fn border_stroke(ui: &egui::Ui) -> Stroke {
        Stroke::new(4.0, ui.visuals().panel_fill)
    }

    pub fn from_profile(
        cache: &'cache mut Images,
        profile: &nostrdb::ProfileRecord<'url>,
    ) -> Option<Self> {
        profile
            .record()
            .profile()
            .and_then(|p| p.picture())
            .map(|url| ProfilePic::new(cache, url))
    }

    #[inline]
    pub fn default_size() -> f32 {
        38.0
    }

    #[inline]
    pub fn medium_size() -> f32 {
        32.0
    }

    #[inline]
    pub fn small_size() -> f32 {
        24.0
    }

    #[inline]
    pub fn no_pfp_url() -> &'static str {
        "https://damus.io/img/no-profile.svg"
    }

    #[inline]
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    #[inline]
    pub fn border(mut self, stroke: Stroke) -> Self {
        self.border = Some(stroke);
        self
    }
}

fn render_pfp(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    url: &str,
    ui_size: f32,
    border: Option<Stroke>,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = 128u32;

    render_images(
        ui,
        img_cache,
        url,
        ImageType::Profile(img_size),
        |ui| {
            paint_circle(ui, ui_size, border);
        },
        |ui, _| {
            paint_circle(ui, ui_size, border);
        },
        |ui, _, renderable_media| {
            let texture_handle = notedeck::get_texture(renderable_media);
            pfp_image(ui, texture_handle, ui_size, border);
        },
    )
}

fn pfp_image(
    ui: &mut egui::Ui,
    img: &TextureHandle,
    size: f32,
    border: Option<Stroke>,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let (rect, response) = ui.allocate_at_least(vec2(size, size), Sense::hover());
    if let Some(stroke) = border {
        draw_bg_border(ui, rect.center(), size, stroke);
    }
    ui.put(rect, egui::Image::new(img).max_width(size));

    response
}

fn paint_circle(ui: &mut egui::Ui, size: f32, border: Option<Stroke>) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), Sense::hover());

    if let Some(stroke) = border {
        draw_bg_border(ui, rect.center(), size, stroke);
    }

    ui.painter()
        .circle_filled(rect.center(), size / 2.0, ui.visuals().weak_text_color());

    response
}

fn draw_bg_border(ui: &mut egui::Ui, center: egui::Pos2, size: f32, stroke: Stroke) {
    let border_size = size + (stroke.width * 2.0);
    ui.painter()
        .circle_filled(center, border_size / 2.0, stroke.color);
}

mod preview {
    use super::*;
    use crate::ui;
    use nostrdb::*;
    use std::collections::HashSet;

    pub struct ProfilePicPreview {
        keys: Option<Vec<ProfileKey>>,
    }

    impl ProfilePicPreview {
        fn new() -> Self {
            ProfilePicPreview { keys: None }
        }

        fn show(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let txn = Transaction::new(app.ndb).unwrap();

                    let keys = if let Some(keys) = &self.keys {
                        keys
                    } else {
                        return;
                    };

                    for key in keys {
                        let profile = app.ndb.get_profile_by_key(&txn, *key).unwrap();
                        let url = profile
                            .record()
                            .profile()
                            .expect("should have profile")
                            .picture()
                            .expect("should have picture");

                        let expand_size = 10.0;
                        let anim_speed = 0.05;

                        let (rect, size, _resp) = ui::anim::hover_expand(
                            ui,
                            egui::Id::new(profile.key().unwrap()),
                            ui::ProfilePic::default_size(),
                            expand_size,
                            anim_speed,
                        );

                        ui.put(
                            rect,
                            ui::ProfilePic::new(app.img_cache, url)
                                .size(size)
                                .border(ui::ProfilePic::border_stroke(ui)),
                        )
                        .on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(300.0);
                            ui.add(ui::ProfilePreview::new(&profile, app.img_cache));
                        });
                    }
                });
            });
        }

        fn setup(&mut self, ndb: &Ndb) {
            let txn = Transaction::new(ndb).unwrap();
            let filters = vec![Filter::new().kinds(vec![0]).build()];
            let mut pks = HashSet::new();
            let mut keys = HashSet::new();

            for query_result in ndb.query(&txn, &filters, 20000).unwrap() {
                pks.insert(query_result.note.pubkey());
            }

            for pk in pks {
                let profile = if let Ok(profile) = ndb.get_profile_by_pubkey(&txn, pk) {
                    profile
                } else {
                    continue;
                };

                if profile
                    .record()
                    .profile()
                    .and_then(|p| p.picture())
                    .is_none()
                {
                    continue;
                }

                keys.insert(profile.key().expect("should not be owned"));
            }

            let keys: Vec<ProfileKey> = keys.into_iter().collect();
            info!("Loaded {} profiles", keys.len());
            self.keys = Some(keys);
        }
    }

    impl notedeck::App for ProfilePicPreview {
        fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
            if self.keys.is_none() {
                self.setup(ctx.ndb);
            }

            self.show(ctx, ui)
        }
    }

    impl Preview for ProfilePic<'_, '_> {
        type Prev = ProfilePicPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            ProfilePicPreview::new()
        }
    }
}
