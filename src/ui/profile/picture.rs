use crate::imgcache::ImageCache;
use crate::ui::{Preview, View};
use egui::{vec2, Sense, TextureHandle};

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut ImageCache,
    url: &'url str,
    size: f32,
}

impl<'cache, 'url> egui::Widget for ProfilePic<'cache, 'url> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_pfp(ui, self.cache, self.url, self.size)
    }
}

impl<'cache, 'url> ProfilePic<'cache, 'url> {
    pub fn new(cache: &'cache mut ImageCache, url: &'url str) -> Self {
        let size = Self::default_size();
        ProfilePic { cache, url, size }
    }

    pub fn default_size() -> f32 {
        32.0
    }

    pub fn no_pfp_url() -> &'static str {
        "https://damus.io/img/no-profile.svg"
    }

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
        let res = crate::images::fetch_img(img_cache, ui.ctx(), url, img_size);
        img_cache.map_mut().insert(url.to_owned(), res);
    }

    match img_cache.map()[url].ready() {
        None => ui.add(egui::Spinner::new().size(ui_size)),

        // Failed to fetch profile!
        Some(Err(_err)) => {
            let m_failed_promise = img_cache.map().get(url);
            if m_failed_promise.is_none() {
                let no_pfp = crate::images::fetch_img(
                    img_cache,
                    ui.ctx(),
                    ProfilePic::no_pfp_url(),
                    img_size,
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
    use nostrdb::*;
    use std::collections::HashSet;

    pub struct ProfilePicPreview {
        cache: ImageCache,
        urls: Vec<String>,
    }

    impl ProfilePicPreview {
        fn new() -> Self {
            let config = Config::new();
            let ndb = Ndb::new(".", &config).expect("ndb");
            let txn = Transaction::new(&ndb).unwrap();
            let filters = vec![Filter::new().kinds(vec![0]).build()];
            let cache = ImageCache::new("cache/img".into());
            let mut pks = HashSet::new();
            let mut urls = HashSet::new();

            for query_result in ndb.query(&txn, filters, 1000).unwrap() {
                pks.insert(query_result.note.pubkey());
            }

            for pk in pks {
                let profile = if let Ok(profile) = ndb.get_profile_by_pubkey(&txn, pk) {
                    profile
                } else {
                    continue;
                };
                if let Some(url) = profile.record().profile().and_then(|p| p.picture()) {
                    urls.insert(url.to_string());
                }
            }

            let urls = urls.into_iter().collect();

            ProfilePicPreview { cache, urls }
        }
    }

    impl View for ProfilePicPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ui.horizontal_wrapped(|ui| {
                for url in &self.urls {
                    ui.add(ProfilePic::new(&mut self.cache, &url));
                }
            });
        }
    }

    impl<'cache, 'url> Preview for ProfilePic<'cache, 'url> {
        type Prev = ProfilePicPreview;

        fn preview() -> Self::Prev {
            ProfilePicPreview::new()
        }
    }
}