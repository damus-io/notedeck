use std::time::SystemTime;

use egui::TextureHandle;

use crate::jobs::MediaJobSender;
use crate::{
    media::{
        gif::{process_gif_frame, AnimatedImgTexCache},
        static_imgs::StaticImgTexCache,
        AnimationMode, BlurCache,
    },
    Error, GifStateMap, ImageType, MediaCacheType, ObfuscationType, TextureState,
};

pub enum MediaRenderState<'a> {
    ActualImage(&'a TextureHandle),
    Transitioning {
        image: &'a TextureHandle,
        obfuscation: ObfuscatedTexture<'a>,
    },
    Error(&'a crate::Error),
    Shimmering(ObfuscatedTexture<'a>),
    Obfuscated(ObfuscatedTexture<'a>),
}

pub enum ObfuscatedTexture<'a> {
    Blur(&'a TextureHandle),
    Default,
}

pub struct NoLoadingLatestTex<'a> {
    static_cache: &'a StaticImgTexCache,
    animated_cache: &'a AnimatedImgTexCache,
    gif_state: &'a mut GifStateMap,
}

impl<'a> NoLoadingLatestTex<'a> {
    pub fn new(
        static_cache: &'a StaticImgTexCache,
        animated_cache: &'a AnimatedImgTexCache,
        gif_state: &'a mut GifStateMap,
    ) -> Self {
        Self {
            static_cache,
            animated_cache,
            gif_state,
        }
    }

    pub fn latest(
        &mut self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        cache_type: MediaCacheType,
        imgtype: ImageType,
        animation_mode: AnimationMode,
    ) -> Option<&'a TextureHandle> {
        let LatestImageTex::Loaded(tex) =
            self.latest_state(jobs, ctx, url, cache_type, imgtype, animation_mode)
        else {
            return None;
        };

        Some(tex)
    }

    pub fn latest_state(
        &mut self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        cache_type: MediaCacheType,
        imgtype: ImageType,
        animation_mode: AnimationMode,
    ) -> LatestImageTex<'a> {
        match cache_type {
            MediaCacheType::Image => {
                match self.static_cache.get_or_request(jobs, ctx, url, imgtype) {
                    TextureState::Pending => LatestImageTex::Pending,
                    TextureState::Error(error) => LatestImageTex::Error(error),
                    TextureState::Loaded(t) => LatestImageTex::Loaded(t),
                }
            }
            MediaCacheType::Gif => {
                match self.animated_cache.get_or_request(jobs, ctx, url, imgtype) {
                    TextureState::Pending => LatestImageTex::Pending,
                    TextureState::Error(error) => LatestImageTex::Error(error),
                    TextureState::Loaded(animation) => {
                        let next_state =
                            process_gif_frame(animation, self.gif_state.get(url), animation_mode);

                        if let Some(new_state) = next_state.maybe_new_state {
                            self.gif_state.insert(url.to_owned(), new_state);
                        }

                        if let Some(repaint) = next_state.repaint_at {
                            tracing::trace!("requesting repaint for {url} after {repaint:?}");
                            if let Ok(dur) = repaint.duration_since(SystemTime::now()) {
                                ctx.request_repaint_after(dur);
                            }
                        }

                        LatestImageTex::Loaded(next_state.texture)
                    }
                }
            }
        }
    }
}

pub enum LatestImageTex<'a> {
    Pending,
    Error(&'a Error),
    Loaded(&'a TextureHandle),
}

pub struct UntrustedMediaLatestTex<'a> {
    blur_cache: &'a BlurCache,
}

/// Media is untrusted and should only show a blur of the underlying media
impl<'a> UntrustedMediaLatestTex<'a> {
    pub fn new(blur_cache: &'a BlurCache) -> Self {
        Self { blur_cache }
    }

    pub fn latest(
        &self,
        jobs: &MediaJobSender,
        ui: &egui::Ui,
        url: &str,
        obfuscation_type: &'a ObfuscationType,
        size: egui::Vec2,
    ) -> MediaRenderState<'a> {
        MediaRenderState::Obfuscated(self.latest_internal(jobs, ui, url, obfuscation_type, size))
    }

    fn latest_internal(
        &self,
        jobs: &MediaJobSender,
        ui: &egui::Ui,
        url: &str,
        obfuscation_type: &'a ObfuscationType,
        size: egui::Vec2,
    ) -> ObfuscatedTexture<'a> {
        // Extract metadata from either ThumbHash or Blurhash variant
        let meta = match obfuscation_type {
            ObfuscationType::ThumbHash(meta) | ObfuscationType::Blurhash(meta) => meta,
            ObfuscationType::Default => return ObfuscatedTexture::Default,
        };

        let state = self.blur_cache.get_or_request(jobs, ui, url, meta, size);

        match &state.tex_state {
            TextureState::Pending | TextureState::Error(_) => ObfuscatedTexture::Default,
            TextureState::Loaded(t) => ObfuscatedTexture::Blur(t),
        }
    }
}

/// Media is trusted and should be loaded ASAP
pub struct TrustedMediaLatestTex<'a> {
    img_no_loading: NoLoadingLatestTex<'a>,
    blur_cache: &'a BlurCache,
}

impl<'a> TrustedMediaLatestTex<'a> {
    pub fn new(img_no_loading: NoLoadingLatestTex<'a>, blur_cache: &'a BlurCache) -> Self {
        Self {
            img_no_loading,
            blur_cache,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn latest(
        &mut self,
        jobs: &MediaJobSender,
        ui: &egui::Ui,
        url: &str,
        cache_type: MediaCacheType,
        imgtype: ImageType,
        animation_mode: AnimationMode,
        obfuscation_type: &'a ObfuscationType,
        size: egui::Vec2,
    ) -> MediaRenderState<'a> {
        let actual_latest_tex = self.img_no_loading.latest_state(
            jobs,
            ui.ctx(),
            url,
            cache_type,
            imgtype,
            animation_mode,
        );

        match actual_latest_tex {
            LatestImageTex::Pending => (),
            LatestImageTex::Error(error) => return MediaRenderState::Error(error),
            LatestImageTex::Loaded(texture_handle) => {
                let Some(blur) = self.blur_cache.get(url) else {
                    return MediaRenderState::ActualImage(texture_handle);
                };

                if blur.finished_transitioning {
                    return MediaRenderState::ActualImage(texture_handle);
                };

                let obfuscation = match &blur.tex_state {
                    TextureState::Pending | TextureState::Error(_) => ObfuscatedTexture::Default,
                    TextureState::Loaded(t) => ObfuscatedTexture::Blur(t),
                };

                return MediaRenderState::Transitioning {
                    image: texture_handle,
                    obfuscation,
                };
            }
        };

        MediaRenderState::Shimmering(
            UntrustedMediaLatestTex::new(self.blur_cache).latest_internal(
                jobs,
                ui,
                url,
                obfuscation_type,
                size,
            ),
        )
    }
}
