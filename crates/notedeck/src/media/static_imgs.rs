use std::path::{Path, PathBuf};

use egui::TextureHandle;
use hashbrown::HashMap;

use crate::{jobs::NoOutputRun, TextureState};
use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, RunType,
    },
    ImageType,
};
use crate::{
    media::{
        images::{
            buffer_to_color_image, get_cached_request_state, parse_img_response, process_image,
            TextureRequestKey,
        },
        load_texture_checked,
        network::http_req,
    },
    MediaCache,
};

pub struct StaticImgTexCache {
    pub(crate) cache: HashMap<TextureRequestKey, TextureState<TextureHandle>>,
    static_img_cache_path: PathBuf,
}

impl StaticImgTexCache {
    pub fn new(static_img_cache_path: PathBuf) -> Self {
        Self {
            cache: Default::default(),
            static_img_cache_path,
        }
    }

    /// Returns true when any size variant for the URL is already in texture memory.
    pub fn contains(&self, url: &str) -> bool {
        self.cache.keys().any(|key| key.url == url)
    }

    pub fn request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
    ) {
        let _ = self.get_or_request(jobs, ctx, url, imgtype);
    }

    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
    ) -> &TextureState<TextureHandle> {
        let request_variant = TextureRequestKey::variant_for_image_type(imgtype);
        if let Some(res) = get_cached_request_state(&self.cache, url, request_variant) {
            return res;
        }
        let request_key = TextureRequestKey::from_variant(url, request_variant);
        let request_id = request_key.to_job_id();

        let key = MediaCache::key(url);
        let path = self.static_img_cache_path.join(key);

        if path.exists() {
            let ctx = ctx.clone();
            let url = url.to_owned();
            let request_key = request_key.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                request_id.clone(),
                MediaJobKind::StaticImg {
                    request_key: request_key.clone(),
                },
                RunType::Output(JobRun::Sync(Box::new(move || {
                    JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(
                        fetch_static_img_from_disk(ctx.clone(), &url, &request_key, imgtype, &path),
                    )))
                }))),
            )) {
                tracing::error!("{e}");
            }
        } else {
            let url = url.to_owned();
            let ctx = ctx.clone();
            let request_key = request_key.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                request_id.clone(),
                MediaJobKind::StaticImg {
                    request_key: request_key.clone(),
                },
                RunType::Output(JobRun::Async(Box::pin(fetch_static_img_from_net(
                    url,
                    request_key,
                    ctx,
                    self.static_img_cache_path.clone(),
                    imgtype,
                )))),
            )) {
                tracing::error!("{e}");
            }
        }

        &TextureState::Pending
    }
}

/// Loads a cached static image, resizing only when the stored image exceeds the requested [`ImageType`].
pub fn fetch_static_img_from_disk(
    ctx: egui::Context,
    url: &str,
    request_key: &TextureRequestKey,
    img_type: ImageType,
    path: &Path,
) -> Result<egui::TextureHandle, crate::Error> {
    tracing::trace!("Starting job static img from disk for {url}");
    let data = std::fs::read(path)?;
    let img = match image::load_from_memory(&data).map_err(crate::Error::Image) {
        Ok(image_buffer) => {
            let width = image_buffer.width();
            let height = image_buffer.height();
            if needs_resize(img_type, width, height) {
                process_image(img_type, image_buffer)
            } else {
                buffer_to_color_image(image_buffer.as_flat_samples_u8(), width, height)
            }
        }
        Err(image_err) => {
            tracing::debug!("raw cache decode failed, trying svg fallback: {image_err}");
            // SVGs are cached as source bytes and rasterized per-request.
            parse_img_response(
                crate::media::network::HyperHttpResponse {
                    content_type: Some("image/svg+xml".to_owned()),
                    bytes: data,
                },
                img_type,
            )?
        }
    };

    Ok(load_texture_checked(
        &ctx,
        request_key.to_job_id(),
        img,
        Default::default(),
    ))
}

async fn fetch_static_img_from_net(
    url: String,
    request_key: TextureRequestKey,
    ctx: egui::Context,
    path: PathBuf,
    imgtype: ImageType,
) -> JobOutput<MediaJobResult> {
    tracing::trace!("fetch static img from net: starting job. sending http request for {url}");
    let res = match http_req(&url).await {
        Ok(r) => r,
        Err(e) => {
            return JobOutput::complete(MediaJobResult::StaticImg(Err(crate::Error::Generic(
                format!("Http error: {e}"),
            ))));
        }
    };

    tracing::trace!("static img from net: parsing http request from {url}");
    JobOutput::Next(JobRun::Sync(Box::new(move || {
        let display_img = match parse_img_response(res, imgtype) {
            Ok(i) => i,
            Err(e) => {
                return JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(Err(
                    e,
                ))))
            }
        };

        let display_texture_handle = load_texture_checked(
            &ctx,
            request_key.to_job_id(),
            display_img.clone(),
            Default::default(),
        );

        JobOutput::Complete(
            CompleteResponse::new(MediaJobResult::StaticImg(Ok(display_texture_handle)))
                .run_no_output(NoOutputRun::Sync(Box::new(move || {
                    tracing::trace!("static img from net: Saving output from {url}");
                    if let Err(e) = MediaCache::write(&path, &url, display_img) {
                        tracing::error!("{e}");
                    }
                }))),
        )
    })))
}

fn needs_resize(img_type: ImageType, width: u32, height: u32) -> bool {
    match img_type {
        ImageType::Profile(size) => width > size || height > size,
        ImageType::Content(Some(dimensions)) => width > dimensions.x || height > dimensions.y,
        ImageType::Content(None) => false,
    }
}
