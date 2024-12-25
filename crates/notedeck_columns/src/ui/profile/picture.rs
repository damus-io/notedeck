use crate::images::ImageType;
use crate::ui::{Preview, PreviewConfig};
use egui::{vec2, Sense, TextureHandle};
use nostrdb::{Ndb, ProfileKey, Transaction};
use tracing::{debug, info};

use notedeck::{AppContext, ImageCache};
use notedeck_ui::PanZoomArea;

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut ImageCache,
    url: &'url str,
    size: f32,
}

impl egui::Widget for ProfilePic<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_pfp(ui, self.cache, self.url, self.size)
    }
}

impl<'cache, 'url> ProfilePic<'cache, 'url> {
    pub fn new(cache: &'cache mut ImageCache, url: &'url str) -> Self {
        let size = Self::default_size();
        ProfilePic { cache, url, size }
    }

    pub fn from_profile(
        cache: &'cache mut ImageCache,
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
}

fn render_pfp(
    ui: &mut egui::Ui,
    img_cache: &mut ImageCache,
    url: &str,
    ui_size: f32,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = 128u32;

    let m_cached_promise = img_cache.map().get(url);
    if m_cached_promise.is_none() {
        let res = crate::images::fetch_img(img_cache, ui.ctx(), url, ImageType::Profile(img_size));
        img_cache.map_mut().insert(url.to_owned(), res);
    }

    match img_cache.map()[url].ready() {
        None => paint_circle(ui, ui_size),

        // Failed to fetch profile!
        Some(Err(_err)) => {
            let m_failed_promise = img_cache.map().get(url);
            if m_failed_promise.is_none() {
                let no_pfp = crate::images::fetch_img(
                    img_cache,
                    ui.ctx(),
                    ProfilePic::no_pfp_url(),
                    ImageType::Profile(img_size),
                );
                img_cache.map_mut().insert(url.to_owned(), no_pfp);
            }

            match img_cache.map().get(url).unwrap().ready() {
                None => paint_circle(ui, ui_size),
                Some(Err(_e)) => {
                    //error!("Image load error: {:?}", e);
                    paint_circle(ui, ui_size)
                }
                Some(Ok(img)) => pfp_image(ui, img, ui_size),
            }
        }
        Some(Ok(img)) => pfp_image(ui, img, ui_size),
    }
}

fn pfp_image(ui: &mut egui::Ui, img: &TextureHandle, size: f32) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //img.show_max_size(ui, egui::vec2(size, size))
    ui.add(egui::Image::new(img).max_width(size))
    //.with_options()
}

fn paint_circle(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, ui.visuals().weak_text_color());

    response
}

mod preview {
    use super::*;
    use crate::ui;
    use nostrdb::Filter;
    use std::collections::HashSet;

    #[derive(PartialEq, Eq, Debug, Hash)]
    struct ProfileCache {
        key: ProfileKey,
        url: String,
    }

    impl ProfileCache {
        pub fn new(key: ProfileKey, url: String) -> Self {
            ProfileCache { key, url }
        }
    }

    pub struct ProfilePicPreview {
        profiles: Option<Vec<ProfileCache>>,
    }

    impl ProfilePicPreview {
        fn new() -> Self {
            ProfilePicPreview { profiles: None }
        }

        fn show(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("viz");

            PanZoomArea::new().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let mut clipped = 0;

                    let profiles = if let Some(profiles) = self.profiles.as_ref() {
                        profiles
                    } else {
                        return;
                    };

                    {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("profile pictures");
                        for profile in profiles {
                            /*
                            let expand_size = 10.0;
                            let anim_speed = 0.05;

                            let (rect, size, _resp) = ui::anim::hover_expand(
                                ui,
                                egui::Id::new(profile.key),
                                ui::ProfilePic::default_size(),
                                expand_size,
                                anim_speed,
                            );
                            */

                            let size = ui::ProfilePic::default_size();
                            let mut rect = ui.available_rect_before_wrap();
                            rect.set_width(size);
                            rect.set_height(size);

                            if rect.max.x > ui.max_rect().max.x {
                                rect = rect.translate(egui::vec2(-rect.min.x, size));
                            }

                            if ui.is_rect_visible(rect) {
                                ui.put(
                                    rect,
                                    ui::ProfilePic::new(app.img_cache, &profile.url).size(size),
                                )
                                .on_hover_ui_at_pointer(|ui| {
                                    ui.set_max_width(300.0);
                                    let txn = Transaction::new(app.ndb).unwrap();
                                    let profile =
                                        app.ndb.get_profile_by_key(&txn, profile.key).unwrap();
                                    ui.add(ui::ProfilePreview::new(&profile, app.img_cache));
                                });
                            } else {
                                ui.allocate_space(rect.size());
                                clipped += 1;
                            }
                        }
                    }

                    debug!("clipped {} profile pics", clipped);
                });
            });
        }

        fn setup(&mut self, ndb: &Ndb) {
            let txn = Transaction::new(ndb).unwrap();
            let filters = vec![Filter::new().kinds(vec![0]).build()];
            let mut pks = HashSet::new();
            let mut profiles: HashSet<ProfileCache> = HashSet::new();

            for query_result in ndb.query(&txn, &filters, 20000).unwrap() {
                pks.insert(query_result.note.pubkey());
            }

            for pk in pks {
                let profile = if let Ok(profile) = ndb.get_profile_by_pubkey(&txn, pk) {
                    profile
                } else {
                    continue;
                };

                let url = profile.record().profile().and_then(|p| p.picture());
                if url.is_none() {
                    continue;
                }
                let url = url.unwrap();

                profiles.insert(ProfileCache::new(profile.key().unwrap(), url.to_owned()));
            }

            let profiles: Vec<ProfileCache> = profiles.into_iter().collect();
            info!("Loaded {} profiles", profiles.len());
            self.profiles = Some(profiles);
        }
    }

    impl notedeck::App for ProfilePicPreview {
        fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
            if self.profiles.is_none() {
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
