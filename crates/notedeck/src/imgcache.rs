use crate::jobs::MediaJobSender;
use crate::media::gif::AnimatedImgTexCache;
use crate::media::images::ImageType;
use crate::media::static_imgs::StaticImgTexCache;
use crate::media::{
    AnimationMode, BlurCache, NoLoadingLatestTex, TrustedMediaLatestTex, UntrustedMediaLatestTex,
};
use crate::urls::{UrlCache, UrlMimes};
use crate::ImageMetadata;
use crate::ObfuscationType;
use crate::RenderableMedia;
use crate::Result;
use egui::TextureHandle;
use image::{Delay, Frame};

use egui::ColorImage;
use webp::AnimFrame;

use std::collections::HashMap;
use std::fs::{self, create_dir_all, File};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::{io, thread};

use hex::ToHex;
use sha2::Digest;
use std::path::PathBuf;
use std::path::{self, Path};
use tracing::warn;

pub struct TexturesCache {
    pub static_image: StaticImgTexCache,
    pub blurred: BlurCache,
    pub animated: AnimatedImgTexCache,
    pub webp: crate::media::webp::WebpTexCache,
}

impl TexturesCache {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            static_image: StaticImgTexCache::new(
                base_dir.join(MediaCache::rel_dir(MediaCacheType::Image)),
            ),
            blurred: Default::default(),
            animated: AnimatedImgTexCache::new(
                base_dir.join(MediaCache::rel_dir(MediaCacheType::Gif)),
            ),
            webp: crate::media::webp::WebpTexCache::new(
                base_dir.join(MediaCache::rel_dir(MediaCacheType::Webp)),
            ),
        }
    }
}

pub enum TextureState<T> {
    Pending,
    Error(crate::Error),
    Loaded(T),
}

impl<T> TextureState<T> {
    pub fn is_loaded(&self) -> bool {
        matches!(self, TextureState::Loaded(_))
    }
}

impl<T> std::fmt::Debug for TextureState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Error(_) => f.debug_tuple("Error").field(&"").finish(),
            Self::Loaded(_) => f.debug_tuple("Loaded").field(&"").finish(),
        }
    }
}

pub struct Animation {
    pub first_frame: TextureFrame,
    pub other_frames: Vec<TextureFrame>,
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
    pub cache_type: MediaCacheType,
    pub cache_size: Arc<Mutex<Option<u64>>>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum MediaCacheType {
    Image,
    Gif,
    Webp,
}

impl MediaCache {
    pub fn new(parent_dir: &Path, cache_type: MediaCacheType) -> Self {
        let cache_dir = parent_dir.join(Self::rel_dir(cache_type));

        let cache_dir_clone = cache_dir.clone();
        let cache_size = Arc::new(Mutex::new(None));
        let cache_size_clone = Arc::clone(&cache_size);

        thread::spawn(move || {
            let mut last_checked = Instant::now() - Duration::from_secs(999);
            loop {
                // check cache folder size every 60 s
                if last_checked.elapsed() >= Duration::from_secs(60) {
                    let size = compute_folder_size(&cache_dir_clone);
                    *cache_size_clone.lock().unwrap() = Some(size);
                    last_checked = Instant::now();
                }
                thread::sleep(Duration::from_secs(5));
            }
        });

        Self {
            cache_dir,
            cache_type,
            cache_size,
        }
    }

    pub fn rel_dir(cache_type: MediaCacheType) -> &'static str {
        match cache_type {
            MediaCacheType::Image => "img",
            MediaCacheType::Gif => "gif",
            MediaCacheType::Webp => "webp",
        }
    }

    pub fn write(cache_dir: &path::Path, url: &str, data: ColorImage) -> Result<()> {
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
            create_dir_all(p)?;
        }
        Ok(File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)?)
    }

    pub fn write_gif(cache_dir: &path::Path, url: &str, data: Vec<ImageFrame>) -> Result<()> {
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

    pub fn write_webp(cache_dir: &path::Path, url: &str, data: Vec<ImageFrame>) -> Result<()> {
        if data.is_empty() {
            return Err(crate::Error::Generic(
                "No frames provided to write_webp".to_owned(),
            ));
        }

        let file_path = cache_dir.join(Self::key(url));
        if let Some(p) = file_path.parent() {
            create_dir_all(p)?;
        }

        // TODO: makes sense to make it static
        let mut config = webp::WebPConfig::new().or(Err(crate::Error::Generic(
            "Failed to configure webp encoder".to_owned(),
        )))?;
        config.lossless = 1;
        config.alpha_compression = 0;

        let reference_frame: &ImageFrame = data.first().ok_or(crate::Error::Generic(
            "No frames provided to write_webp".to_owned(),
        ))?;
        let mut encoder = webp::AnimEncoder::new(
            reference_frame.image.size[0] as u32,
            reference_frame.image.size[1] as u32,
            &config,
        );

        let mut timestamp = 0i32;
        for frame in data.iter() {
            let [width, height] = frame.image.size;
            let delay = frame.delay.as_millis();
            let frame_delay = if delay <= i32::MAX as u128 {
                delay as i32
            } else {
                300i32
            };

            encoder.add_frame(AnimFrame::from_rgba(
                frame.image.as_raw(),
                width as u32,
                height as u32,
                timestamp,
            ));

            timestamp = timestamp.saturating_add(frame_delay);
        }

        let webp = encoder.encode();

        Ok(std::fs::write(file_path, &*webp)?)
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

    fn clear(&mut self) {
        *self.cache_size.try_lock().unwrap() = Some(0);
    }
}

fn color_image_to_rgba(color_image: ColorImage) -> image::RgbaImage {
    let width = color_image.width() as u32;
    let height = color_image.height() as u32;

    let rgba_pixels: Vec<u8> = color_image
        .pixels
        .iter()
        .flat_map(|color| color.to_array()) // Convert Color32 to `[u8; 4]`
        .collect();

    image::RgbaImage::from_raw(width, height, rgba_pixels)
        .expect("Failed to create RgbaImage from ColorImage")
}

fn compute_folder_size<P: AsRef<Path>>(path: P) -> u64 {
    fn walk(path: &Path) -> u64 {
        let mut size = 0;
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        size += metadata.len();
                    } else if metadata.is_dir() {
                        size += walk(&path);
                    }
                }
            }
        }
        size
    }
    walk(path.as_ref())
}

pub struct Images {
    pub base_path: path::PathBuf,
    pub static_imgs: MediaCache,
    pub gifs: MediaCache,
    pub webps: MediaCache,
    pub textures: TexturesCache,
    pub urls: UrlMimes,
    /// cached imeta data
    pub metadata: HashMap<String, ImageMetadata>,
    pub gif_states: GifStateMap,
    pub webp_states: WebpStateMap,
}

impl Images {
    /// path to directory to place [`MediaCache`]s
    pub fn new(path: path::PathBuf) -> Self {
        Self {
            base_path: path.clone(),
            static_imgs: MediaCache::new(&path, MediaCacheType::Image),
            gifs: MediaCache::new(&path, MediaCacheType::Gif),
            webps: MediaCache::new(&path, MediaCacheType::Webp),
            urls: UrlMimes::new(UrlCache::new(path.join(UrlCache::rel_dir()))),
            gif_states: Default::default(),
            webp_states: Default::default(),
            metadata: Default::default(),
            textures: TexturesCache::new(path.clone()),
        }
    }

    pub fn migrate_v0(&self) -> Result<()> {
        self.static_imgs.migrate_v0()?;
        self.gifs.migrate_v0()?;
        self.webps.migrate_v0()
    }

    pub fn get_renderable_media(&mut self, url: &str) -> Option<RenderableMedia> {
        Self::find_renderable_media(&mut self.urls, &self.metadata, url)
    }

    pub fn find_renderable_media(
        urls: &mut UrlMimes,
        imeta: &HashMap<String, ImageMetadata>,
        url: &str,
    ) -> Option<RenderableMedia> {
        let media_type = crate::urls::supported_mime_hosted_at_url(urls, url)?;

        let obfuscation_type = match imeta.get(url) {
            Some(blur) => ObfuscationType::Blurhash(blur.clone()),
            None => ObfuscationType::Default,
        };

        Some(RenderableMedia {
            url: url.to_string(),
            media_type,
            obfuscation_type,
        })
    }

    pub fn latest_texture<'a>(
        &'a mut self,
        jobs: &MediaJobSender,
        ui: &mut egui::Ui,
        url: &str,
        img_type: ImageType,
        animation_mode: AnimationMode,
    ) -> Option<&'a TextureHandle> {
        let cache_type = crate::urls::supported_mime_hosted_at_url(&mut self.urls, url)?;

        let mut loader = NoLoadingLatestTex::new(
            &self.textures.static_image,
            &self.textures.animated,
            &self.textures.webp,
            &mut self.gif_states,
            &mut self.webp_states,
        );
        loader.latest(jobs, ui.ctx(), url, cache_type, img_type, animation_mode)
    }

    pub fn get_cache(&self, cache_type: MediaCacheType) -> &MediaCache {
        match cache_type {
            MediaCacheType::Image => &self.static_imgs,
            MediaCacheType::Gif => &self.gifs,
            MediaCacheType::Webp => &self.webps,
        }
    }

    pub fn get_cache_mut(&mut self, cache_type: MediaCacheType) -> &mut MediaCache {
        match cache_type {
            MediaCacheType::Image => &mut self.static_imgs,
            MediaCacheType::Gif => &mut self.gifs,
            MediaCacheType::Webp => &mut self.webps,
        }
    }

    pub fn clear_folder_contents(&mut self) -> io::Result<()> {
        for entry in fs::read_dir(self.base_path.clone())? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }

        self.urls.cache.clear();
        self.static_imgs.clear();
        self.gifs.clear();
        self.webps.clear();
        self.gif_states.clear();
        self.webp_states.clear();

        Ok(())
    }

    pub fn trusted_texture_loader(&mut self) -> TrustedMediaLatestTex<'_> {
        TrustedMediaLatestTex::new(
            NoLoadingLatestTex::new(
                &self.textures.static_image,
                &self.textures.animated,
                &self.textures.webp,
                &mut self.gif_states,
                &mut self.webp_states,
            ),
            &self.textures.blurred,
        )
    }

    pub fn untrusted_texture_loader(&mut self) -> UntrustedMediaLatestTex<'_> {
        UntrustedMediaLatestTex::new(&self.textures.blurred)
    }

    pub fn no_img_loading_tex_loader(&'_ mut self) -> NoLoadingLatestTex<'_> {
        NoLoadingLatestTex::new(
            &self.textures.static_image,
            &self.textures.animated,
            &self.textures.webp,
            &mut self.gif_states,
            &mut self.webp_states,
        )
    }

    pub fn user_trusts_img(&self, url: &str, media_type: MediaCacheType) -> bool {
        match media_type {
            MediaCacheType::Image => self.textures.static_image.contains(url),
            MediaCacheType::Gif => self.textures.animated.contains(url),
            MediaCacheType::Webp => self.textures.webp.contains(url),
        }
    }
}

pub type GifStateMap = HashMap<String, GifState>;

pub struct GifState {
    pub last_frame_rendered: Instant,
    pub last_frame_duration: Duration,
    pub next_frame_time: Option<SystemTime>,
    pub last_frame_index: usize,
}

pub type WebpStateMap = HashMap<String, WebpState>;

pub struct WebpState {
    pub last_frame_rendered: Instant,
    pub last_frame_duration: Duration,
    pub next_frame_time: Option<SystemTime>,
    pub last_frame_index: usize,
}

pub struct LatestTexture {
    pub texture: TextureHandle,
    pub request_next_repaint: Option<SystemTime>,
}
