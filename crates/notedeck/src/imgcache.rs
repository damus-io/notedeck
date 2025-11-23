use crate::media::gif::ensure_latest_texture_from_cache;
use crate::media::images::ImageType;
use crate::media::AnimationMode;
use crate::urls::{UrlCache, UrlMimes};
use crate::ImageMetadata;
use crate::ObfuscationType;
use crate::RenderableMedia;
use crate::Result;
use egui::TextureHandle;
use image::{Delay, Frame};
use poll_promise::Promise;

use egui::ColorImage;

use std::collections::HashMap;
use std::fs::{self, create_dir_all, File};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::{io, thread};

use hex::ToHex;
use sha2::Digest;
use std::path::PathBuf;
use std::path::{self, Path};
use tracing::warn;

#[derive(Default)]
pub struct TexturesCache {
    pub cache: hashbrown::HashMap<String, TextureStateInternal>,
}

impl TexturesCache {
    pub fn handle_and_get_or_insert_loadable(
        &mut self,
        url: &str,
        closure: impl FnOnce() -> Promise<Option<Result<TexturedImage>>>,
    ) -> LoadableTextureState<'_> {
        let internal = self.handle_and_get_state_internal(url, true, closure);

        internal.into()
    }

    pub fn handle_and_get_or_insert(
        &mut self,
        url: &str,
        closure: impl FnOnce() -> Promise<Option<Result<TexturedImage>>>,
    ) -> TextureStateOld<'_> {
        let internal = self.handle_and_get_state_internal(url, false, closure);

        internal.into()
    }

    fn handle_and_get_state_internal(
        &mut self,
        url: &str,
        use_loading: bool,
        closure: impl FnOnce() -> Promise<Option<Result<TexturedImage>>>,
    ) -> &mut TextureStateInternal {
        let state = match self.cache.raw_entry_mut().from_key(url) {
            hashbrown::hash_map::RawEntryMut::Occupied(entry) => {
                let state = entry.into_mut();
                handle_occupied(state, use_loading);

                state
            }
            hashbrown::hash_map::RawEntryMut::Vacant(entry) => {
                let res = closure();
                let (_, state) = entry.insert(url.to_owned(), TextureStateInternal::Pending(res));

                state
            }
        };

        state
    }

    pub fn insert_pending(&mut self, url: &str, promise: Promise<Option<Result<TexturedImage>>>) {
        self.cache
            .insert(url.to_owned(), TextureStateInternal::Pending(promise));
    }

    pub fn move_to_loaded(&mut self, url: &str) {
        let hashbrown::hash_map::RawEntryMut::Occupied(entry) =
            self.cache.raw_entry_mut().from_key(url)
        else {
            return;
        };

        entry.replace_entry_with(|_, v| {
            let TextureStateInternal::Loading(textured) = v else {
                return Some(v);
            };

            Some(TextureStateInternal::Loaded(textured))
        });
    }

    pub fn get_and_handle(&mut self, url: &str) -> Option<LoadableTextureState<'_>> {
        self.cache.get_mut(url).map(|state| {
            handle_occupied(state, true);
            state.into()
        })
    }
}

fn handle_occupied(state: &mut TextureStateInternal, use_loading: bool) {
    let TextureStateInternal::Pending(promise) = state else {
        return;
    };

    let Some(res) = promise.ready_mut() else {
        return;
    };

    let Some(res) = res.take() else {
        tracing::error!("Failed to take the promise");
        *state =
            TextureStateInternal::Error(crate::Error::Generic("Promise already taken".to_owned()));
        return;
    };

    match res {
        Ok(textured) => {
            *state = if use_loading {
                TextureStateInternal::Loading(textured)
            } else {
                TextureStateInternal::Loaded(textured)
            }
        }
        Err(e) => *state = TextureStateInternal::Error(e),
    }
}

pub enum LoadableTextureState<'a> {
    Pending,
    Error(&'a crate::Error),
    Loading {
        actual_image_tex: &'a mut TexturedImage,
    }, // the texture is in the loading state, for transitioning between the pending and loaded states
    Loaded(&'a mut TexturedImage),
}

pub enum TextureStateOld<'a> {
    Pending,
    Error(&'a crate::Error),
    Loaded(&'a mut TexturedImage),
}

impl<'a> TextureStateOld<'a> {
    pub fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }
}

pub enum TextureState<T> {
    Pending,
    Error(crate::Error),
    Loaded(T),
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

impl<'a> From<&'a mut TextureStateInternal> for TextureStateOld<'a> {
    fn from(value: &'a mut TextureStateInternal) -> Self {
        match value {
            TextureStateInternal::Pending(_) => TextureStateOld::Pending,
            TextureStateInternal::Error(error) => TextureStateOld::Error(error),
            TextureStateInternal::Loading(textured_image) => {
                TextureStateOld::Loaded(textured_image)
            }
            TextureStateInternal::Loaded(textured_image) => TextureStateOld::Loaded(textured_image),
        }
    }
}

pub enum TextureStateInternal {
    Pending(Promise<Option<Result<TexturedImage>>>),
    Error(crate::Error),
    Loading(TexturedImage), // the image is in the loading state, for transitioning between blur and image
    Loaded(TexturedImage),
}

impl<'a> From<&'a mut TextureStateInternal> for LoadableTextureState<'a> {
    fn from(value: &'a mut TextureStateInternal) -> Self {
        match value {
            TextureStateInternal::Pending(_) => LoadableTextureState::Pending,
            TextureStateInternal::Error(error) => LoadableTextureState::Error(error),
            TextureStateInternal::Loading(textured_image) => LoadableTextureState::Loading {
                actual_image_tex: textured_image,
            },
            TextureStateInternal::Loaded(textured_image) => {
                LoadableTextureState::Loaded(textured_image)
            }
        }
    }
}

pub enum TexturedImage {
    Static(TextureHandle),
    Animated(Animation),
}

impl TexturedImage {
    pub fn get_first_texture(&self) -> &TextureHandle {
        match self {
            TexturedImage::Static(texture_handle) => texture_handle,
            TexturedImage::Animated(animation) => &animation.first_frame.texture,
        }
    }
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
    pub textures_cache: TexturesCache,
    pub cache_type: MediaCacheType,
    pub cache_size: Arc<Mutex<Option<u64>>>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum MediaCacheType {
    Image,
    Gif,
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
            textures_cache: TexturesCache::default(),
            cache_type,
            cache_size,
        }
    }

    pub fn rel_dir(cache_type: MediaCacheType) -> &'static str {
        match cache_type {
            MediaCacheType::Image => "img",
            MediaCacheType::Gif => "gif",
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
        self.textures_cache.cache.clear();
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
    pub urls: UrlMimes,
    /// cached imeta data
    pub metadata: HashMap<String, ImageMetadata>,
    pub gif_states: GifStateMap,
}

impl Images {
    /// path to directory to place [`MediaCache`]s
    pub fn new(path: path::PathBuf) -> Self {
        Self {
            base_path: path.clone(),
            static_imgs: MediaCache::new(&path, MediaCacheType::Image),
            gifs: MediaCache::new(&path, MediaCacheType::Gif),
            urls: UrlMimes::new(UrlCache::new(path.join(UrlCache::rel_dir()))),
            gif_states: Default::default(),
            metadata: Default::default(),
        }
    }

    pub fn migrate_v0(&self) -> Result<()> {
        self.static_imgs.migrate_v0()?;
        self.gifs.migrate_v0()
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

    pub fn latest_texture(
        &mut self,
        ui: &mut egui::Ui,
        url: &str,
        img_type: ImageType,
        animation_mode: AnimationMode,
    ) -> Option<TextureHandle> {
        let cache_type = crate::urls::supported_mime_hosted_at_url(&mut self.urls, url)?;

        let cache_dir = self.get_cache(cache_type).cache_dir.clone();
        let is_loaded = self
            .get_cache_mut(cache_type)
            .textures_cache
            .handle_and_get_or_insert(url, || {
                crate::media::images::fetch_img(&cache_dir, ui.ctx(), url, img_type, cache_type)
            })
            .is_loaded();

        if !is_loaded {
            return None;
        }

        let cache = match cache_type {
            MediaCacheType::Image => &mut self.static_imgs,
            MediaCacheType::Gif => &mut self.gifs,
        };

        ensure_latest_texture_from_cache(
            ui,
            url,
            &mut self.gif_states,
            &mut cache.textures_cache,
            animation_mode,
        )
    }

    pub fn get_cache(&self, cache_type: MediaCacheType) -> &MediaCache {
        match cache_type {
            MediaCacheType::Image => &self.static_imgs,
            MediaCacheType::Gif => &self.gifs,
        }
    }

    pub fn get_cache_mut(&mut self, cache_type: MediaCacheType) -> &mut MediaCache {
        match cache_type {
            MediaCacheType::Image => &mut self.static_imgs,
            MediaCacheType::Gif => &mut self.gifs,
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
        self.gif_states.clear();

        Ok(())
    }
}

pub type GifStateMap = HashMap<String, GifState>;

pub struct GifState {
    pub last_frame_rendered: Instant,
    pub last_frame_duration: Duration,
    pub next_frame_time: Option<SystemTime>,
    pub last_frame_index: usize,
}

pub struct LatestTexture {
    pub texture: TextureHandle,
    pub request_next_repaint: Option<SystemTime>,
}

#[profiling::function]
pub fn get_render_state<'a>(
    ctx: &egui::Context,
    images: &'a mut Images,
    cache_type: MediaCacheType,
    url: &str,
    img_type: ImageType,
) -> RenderState<'a> {
    let cache = match cache_type {
        MediaCacheType::Image => &mut images.static_imgs,
        MediaCacheType::Gif => &mut images.gifs,
    };

    let texture_state = cache.textures_cache.handle_and_get_or_insert(url, || {
        crate::media::images::fetch_img(&cache.cache_dir, ctx, url, img_type, cache_type)
    });

    RenderState {
        texture_state,
        gifs: &mut images.gif_states,
    }
}

pub struct RenderState<'a> {
    pub texture_state: TextureStateOld<'a>,
    pub gifs: &'a mut GifStateMap,
}
