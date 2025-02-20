use notedeck::{GifStateMap, Images, MediaCache, MediaCacheType, TexturedImage};

use crate::images::ImageType;

use super::ProfilePic;

#[allow(clippy::too_many_arguments)]
pub fn render_images(
    ui: &mut egui::Ui,
    images: &mut Images,
    url: &str,
    img_type: ImageType,
    cache_type: MediaCacheType,
    show_waiting: impl FnOnce(&mut egui::Ui),
    show_error: impl FnOnce(&mut egui::Ui, String),
    show_success: impl FnOnce(&mut egui::Ui, &str, &mut TexturedImage, &mut GifStateMap),
) -> egui::Response {
    let cache = match cache_type {
        MediaCacheType::Image => &mut images.static_imgs,
        MediaCacheType::Gif => &mut images.gifs,
    };

    render_media_cache(
        ui,
        cache,
        &mut images.gif_states,
        url,
        img_type,
        cache_type,
        show_waiting,
        show_error,
        show_success,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_media_cache(
    ui: &mut egui::Ui,
    cache: &mut MediaCache,
    gif_states: &mut GifStateMap,
    url: &str,
    img_type: ImageType,
    cache_type: MediaCacheType,
    show_waiting: impl FnOnce(&mut egui::Ui),
    show_error: impl FnOnce(&mut egui::Ui, String),
    show_success: impl FnOnce(&mut egui::Ui, &str, &mut TexturedImage, &mut GifStateMap),
) -> egui::Response {
    let m_cached_promise = cache.map().get(url);

    if m_cached_promise.is_none() {
        let res = crate::images::fetch_img(cache, ui.ctx(), url, img_type, cache_type.clone());
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
                        cache_type,
                    );
                    cache.map_mut().insert(url.to_owned(), no_pfp);
                    show_error(ui, err)
                }
                Some(Ok(renderable_media)) => show_success(ui, url, renderable_media, gif_states),
            }
        })
        .response
}
