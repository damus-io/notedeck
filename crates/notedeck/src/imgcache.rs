use crate::Result;
use egui::TextureHandle;
use poll_promise::Promise;

use egui::ColorImage;

use std::collections::HashMap;
use std::fs::{create_dir_all, File};

use hex::ToHex;
use sha2::Digest;
use std::path;
use std::path::PathBuf;

pub type ImageCacheValue = Promise<Result<TextureHandle>>;
pub type ImageCacheMap = HashMap<String, ImageCacheValue>;

pub struct ImageCache {
    pub cache_dir: path::PathBuf,
    url_imgs: ImageCacheMap,
}

impl ImageCache {
    pub fn new(cache_dir: path::PathBuf) -> Self {
        Self {
            cache_dir,
            url_imgs: HashMap::new(),
        }
    }

    pub fn rel_dir() -> &'static str {
        "img"
    }

    /*
    pub fn fetch(image: &str) -> Result<Image> {
        let m_cached_promise = img_cache.map().get(image);
        if m_cached_promise.is_none() {
            let res = crate::images::fetch_img(
                img_cache,
                ui.ctx(),
                &image,
                ImageType::Content(width.round() as u32, height.round() as u32),
            );
            img_cache.map_mut().insert(image.to_owned(), res);
        }
    }
    */

    pub fn write(cache_dir: &path::Path, url: &str, data: ColorImage) -> Result<()> {
        let file_path = cache_dir.join(Self::key(url));
        if let Some(p) = file_path.parent() {
            create_dir_all(p)?;
        }
        let file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)?;
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(file);

        encoder.encode(
            data.as_raw(),
            data.size[0] as u32,
            data.size[1] as u32,
            image::ColorType::Rgba8.into(),
        )?;

        Ok(())
    }

    pub fn key(url: &str) -> String {
        let k: String = sha2::Sha256::digest(url.as_bytes()).encode_hex();
        PathBuf::from(&k[0..2])
            .join(&k[2..4])
            .join(k)
            .to_string_lossy()
            .to_string()
    }

    pub fn map(&self) -> &ImageCacheMap {
        &self.url_imgs
    }

    pub fn map_mut(&mut self) -> &mut ImageCacheMap {
        &mut self.url_imgs
    }
}
