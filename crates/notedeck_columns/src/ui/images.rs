use notedeck::{Images, MediaCache, MediaCacheType, TexturedImage};

use crate::images::ImageType;

use super::ProfilePic;

pub fn render_images(
    ui: &mut egui::Ui,
    images: &mut Images,
    url: &str,
    img_type: ImageType,
    cache_type: MediaCacheType,
    show_waiting: impl FnOnce(&mut egui::Ui),
    show_error: impl FnOnce(&mut egui::Ui, String),
    show_success: impl FnOnce(&mut egui::Ui, &str, &mut TexturedImage),
) -> egui::Response {
    let cache = match cache_type.clone() {
        MediaCacheType::Image => &mut images.static_imgs,
        MediaCacheType::Gif => &mut images.gifs,
    };

    render_media_cache(
        ui,
        cache,
        url,
        img_type,
        show_waiting,
        show_error,
        show_success,
    )
}

pub fn render_media_cache(
    ui: &mut egui::Ui,
    cache: &mut MediaCache,
    url: &str,
    img_type: ImageType,
    show_waiting: impl FnOnce(&mut egui::Ui),
    show_error: impl FnOnce(&mut egui::Ui, String),
    show_success: impl FnOnce(&mut egui::Ui, &str, &mut TexturedImage),
) -> egui::Response {
    let m_cached_promise = cache.map().get(url);

    if m_cached_promise.is_none() {
        let res = crate::images::fetch_img(cache, ui.ctx(), url, img_type);
        cache.map_mut().insert(url.to_owned(), res);
    }

    egui::Frame::none()
        .show(ui, |ui| {
            match cache.map_mut().get_mut(url).and_then(|p| p.ready_mut()) {
                None => show_waiting(ui),
                Some(Err(err)) => {
                    let err = err.to_string();
                    let no_pfp = crate::images::fetch_img(
                        cache,
                        ui.ctx(),
                        ProfilePic::no_pfp_url(),
                        ImageType::Profile(128),
                    );
                    cache.map_mut().insert(url.to_owned(), no_pfp);
                    show_error(ui, err)
                }
                Some(Ok(renderable_media)) => show_success(ui, url, renderable_media),
            }
        })
        .response
}
