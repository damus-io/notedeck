use crate::Result;
use egui::TextureHandle;
use image::{Delay, Frame};
use poll_promise::Promise;

use egui::ColorImage;

use std::collections::HashMap;
use std::fs::{create_dir_all, File};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use hex::ToHex;
use sha2::Digest;
use std::path;
use std::path::PathBuf;
use tracing::warn;

pub type MediaCacheValue = Promise<Result<TexturedImage>>;
pub type MediaCacheMap = HashMap<String, MediaCacheValue>;

pub enum TexturedImage {
    Static(TextureHandle),
    Animated(Animation),
}

pub struct Animation {
    pub first_frame: TextureFrame,
    pub other_frames: Vec<TextureFrame>,
    pub receiver: Option<Receiver<TextureFrame>>,
}

impl Animation {
    pub fn get_frame(&self, index: usize) -> Option<&TextureFrame> {
        if index == 0 {
            Some(&self.first_frame)
        } else {
            self.other_frames.get(index - 1)
        }
    }

    pub fn num_frames(&self) -> usize {
        self.other_frames.len() + 1
    }
}

pub struct TextureFrame {
    pub delay: Duration,
    pub texture: TextureHandle,
}

pub struct ImageFrame {
    pub delay: Duration,
    pub image: ColorImage,
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
        // tracing::info!("Writing static image");
        let file = Self::create_file(cache_dir, url)?;
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(file);

        encoder.encode(
            data.as_raw(),
            data.size[0] as u32,
            data.size[1] as u32,
            image::ColorType::Rgba8.into(),
        )?;

        Ok(())
    }

    fn create_file(cache_dir: &path::Path, url: &str) -> Result<File> {
        let file_path = cache_dir.join(Self::key(url));
        if let Some(p) = file_path.parent() {
            // tracing::info!(
            //     "creating new file... cache_dir: {:?}, path: {:?}, path without parent: {:?}",
            //     cache_dir,
            //     file_path,
            //     p
            // );
            create_dir_all(p)?;
        }
        Ok(File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)?)
    }

    pub fn write_gif(cache_dir: &path::Path, url: &str, data: Vec<ImageFrame>) -> Result<()> {
        // tracing::info!("Writing new gif file");
        let file = Self::create_file(cache_dir, url)?;

        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        for img in data {
            let buf = color_image_to_rgba(img.image);
            let frame = Frame::from_parts(buf, 0, 0, Delay::from_saturating_duration(img.delay));
            if let Err(e) = encoder.encode_frame(frame) {
                tracing::error!("problem encoding frame: {e}");
            }
        }

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

// TODO(kernelkind): create test
fn color_image_to_rgba(color_image: ColorImage) -> image::RgbaImage {
    let width = color_image.width() as u32;
    let height = color_image.height() as u32;

    // Convert `Color32` pixels to raw RGBA bytes
    let rgba_pixels: Vec<u8> = color_image
        .pixels
        .iter()
        .flat_map(|color| color.to_array()) // Convert Color32 to `[u8; 4]`
        .collect();

    // Create RgbaImage from raw pixel data
    image::RgbaImage::from_raw(width, height, rgba_pixels)
        .expect("Failed to create RgbaImage from ColorImage")
}
