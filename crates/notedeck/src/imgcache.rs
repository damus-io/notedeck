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
use tracing::warn;

pub type MediaCacheValue = Promise<Result<TexturedImage>>;
pub type MediaCacheMap = HashMap<String, MediaCacheValue>;

pub enum TexturedImage {
    Static(TextureHandle),
}

pub struct MediaCache {
    pub cache_dir: path::PathBuf,
    url_imgs: MediaCacheMap,
}

pub enum MediaCacheType {
    Image,
    Gif,
}

impl MediaCache {
    pub fn new(cache_dir: path::PathBuf) -> Self {
        Self {
            cache_dir,
            url_imgs: HashMap::new(),
        }
    }

    pub fn rel_dir(cache_type: MediaCacheType) -> &'static str {
        match cache_type {
            MediaCacheType::Image => "img",
            MediaCacheType::Gif => "gif",
        }
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

    /// Migrate from base32 encoded url to sha256 url + sub-dir structure
    pub fn migrate_v0(&self) -> Result<()> {
        for file in std::fs::read_dir(&self.cache_dir)? {
            let file = if let Ok(f) = file {
                f
            } else {
                // not sure how this could fail, skip entry
                continue;
            };
            if !file.path().is_file() {
                continue;
            }
            let old_filename = file.file_name().to_string_lossy().to_string();
            let old_url = if let Some(u) =
                base32::decode(base32::Alphabet::Crockford, &old_filename)
                    .and_then(|s| String::from_utf8(s).ok())
            {
                u
            } else {
                warn!("Invalid base32 filename: {}", &old_filename);
                continue;
            };
            let new_path = self.cache_dir.join(Self::key(&old_url));
            if let Some(p) = new_path.parent() {
                create_dir_all(p)?;
            }

            if let Err(e) = std::fs::rename(file.path(), &new_path) {
                warn!(
                    "Failed to migrate file from {} to {}: {:?}",
                    file.path().display(),
                    new_path.display(),
                    e
                );
            }
        }
        Ok(())
    }

    pub fn map(&self) -> &MediaCacheMap {
        &self.url_imgs
    }

    pub fn map_mut(&mut self) -> &mut MediaCacheMap {
        &mut self.url_imgs
    }
}

// TODO: temporary...
pub fn get_texture(textured_image: &TexturedImage) -> &TextureHandle {
    match textured_image {
        TexturedImage::Static(texture_handle) => texture_handle,
    }
}
