use crate::Result;
use egui::TextureHandle;
use poll_promise::Promise;

use egui::ColorImage;

use std::collections::HashMap;
use std::fs::File;

use std::path;

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

    pub fn write(cache_dir: &path::Path, url: &str, data: ColorImage) -> Result<()> {
        let file_path = cache_dir.join(Self::key(url));
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
            image::ColorType::Rgba8,
        )?;

        Ok(())
    }

    pub fn key(url: &str) -> String {
        base32::encode(base32::Alphabet::Crockford, url.as_bytes())
    }

    pub fn map(&self) -> &ImageCacheMap {
        &self.url_imgs
    }

    pub fn map_mut(&mut self) -> &mut ImageCacheMap {
        &mut self.url_imgs
    }
}
