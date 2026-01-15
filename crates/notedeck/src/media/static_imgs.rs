use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use egui::TextureHandle;

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
        images::{buffer_to_color_image, parse_img_response},
        load_texture_checked,
        network::http_fetch,
    },
    MediaCache,
};

/// Configuration for HTTP requests, including optional SOCKS proxy.
#[derive(Clone, Default)]
pub struct HttpConfig {
    pub socks_proxy: Option<String>,
}

pub struct StaticImgTexCache {
    pub(crate) cache: HashMap<String, TextureState<TextureHandle>>,
    static_img_cache_path: PathBuf,
    http_config: HttpConfig,
}

impl StaticImgTexCache {
    pub fn new(static_img_cache_path: PathBuf) -> Self {
        Self {
            cache: Default::default(),
            static_img_cache_path,
            http_config: HttpConfig::default(),
        }
    }

    /// Update the HTTP configuration (e.g., SOCKS proxy for Tor).
    pub fn set_http_config(&mut self, config: HttpConfig) {
        self.http_config = config;
    }

    pub fn contains(&self, url: &str) -> bool {
        self.cache.contains_key(url)
    }

    pub fn get(&self, url: &str) -> Option<&TextureState<TextureHandle>> {
        self.cache.get(url)
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
        if let Some(res) = self.cache.get(url) {
            return res;
        }

        let key = MediaCache::key(url);
        let path = self.static_img_cache_path.join(key);

        if path.exists() {
            let ctx = ctx.clone();
            let url = url.to_owned();
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::StaticImg,
                RunType::Output(JobRun::Sync(Box::new(move || {
                    JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(
                        fetch_static_img_from_disk(ctx.clone(), &url, &path),
                    )))
                }))),
            )) {
                tracing::error!("{e}");
            }
        } else {
            let url = url.to_owned();
            let ctx = ctx.clone();
            let http_config = self.http_config.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::StaticImg,
                RunType::Output(JobRun::Async(Box::pin(fetch_static_img_from_net(
                    url,
                    ctx,
                    self.static_img_cache_path.clone(),
                    imgtype,
                    http_config,
                )))),
            )) {
                tracing::error!("{e}");
            }
        }

        &TextureState::Pending
    }
}

pub fn fetch_static_img_from_disk(
    ctx: egui::Context,
    url: &str,
    path: &Path,
) -> Result<egui::TextureHandle, crate::Error> {
    tracing::trace!("Starting job static img from disk for {url}");
    let data = std::fs::read(path)?;
    let image_buffer = image::load_from_memory(&data).map_err(crate::Error::Image);

    let image_buffer = match image_buffer {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("could not load img buffer");
            return Err(e);
        }
    };

    let img = buffer_to_color_image(
        image_buffer.as_flat_samples_u8(),
        image_buffer.width(),
        image_buffer.height(),
    );

    Ok(load_texture_checked(&ctx, url, img, Default::default()))
}

async fn fetch_static_img_from_net(
    url: String,
    ctx: egui::Context,
    path: PathBuf,
    imgtype: ImageType,
    http_config: HttpConfig,
) -> JobOutput<MediaJobResult> {
    tracing::trace!("fetch static img from net: starting job. sending http request for {url}");
    let res = match http_fetch(&url, http_config.socks_proxy.as_deref()).await {
        Ok(r) => r,
        Err(e) => {
            return JobOutput::complete(MediaJobResult::StaticImg(Err(crate::Error::Generic(
                format!("Http error: {e}"),
            ))));
        }
    };

    tracing::trace!("static img from net: parsing http request from {url}");
    JobOutput::Next(JobRun::Sync(Box::new(move || {
        let img = match parse_img_response(res, imgtype) {
            Ok(i) => i,
            Err(e) => {
                return JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(Err(
                    e,
                ))))
            }
        };

        let texture_handle =
            load_texture_checked(&ctx, url.clone(), img.clone(), Default::default());

        JobOutput::Complete(
            CompleteResponse::new(MediaJobResult::StaticImg(Ok(texture_handle))).run_no_output(
                NoOutputRun::Sync(Box::new(move || {
                    tracing::trace!("static img from net: Saving output from {url}");
                    if let Err(e) = MediaCache::write(&path, &url, img) {
                        tracing::error!("{e}");
                    }
                })),
            ),
        )
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_config_default() {
        let config = HttpConfig::default();
        assert!(config.socks_proxy.is_none());
    }

    #[test]
    fn test_http_config_with_proxy() {
        let config = HttpConfig {
            socks_proxy: Some("127.0.0.1:9150".to_string()),
        };
        assert_eq!(config.socks_proxy, Some("127.0.0.1:9150".to_string()));
    }

    #[test]
    fn test_http_config_clone() {
        let config = HttpConfig {
            socks_proxy: Some("127.0.0.1:9150".to_string()),
        };
        let cloned = config.clone();
        assert_eq!(config.socks_proxy, cloned.socks_proxy);
    }

    #[test]
    fn test_static_img_tex_cache_set_http_config() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut cache = StaticImgTexCache::new(temp_dir.path().to_path_buf());

        // Initially no proxy
        assert!(cache.http_config.socks_proxy.is_none());

        // Set proxy
        cache.set_http_config(HttpConfig {
            socks_proxy: Some("127.0.0.1:9150".to_string()),
        });
        assert_eq!(
            cache.http_config.socks_proxy,
            Some("127.0.0.1:9150".to_string())
        );

        // Clear proxy
        cache.set_http_config(HttpConfig::default());
        assert!(cache.http_config.socks_proxy.is_none());
    }
}
