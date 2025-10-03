pub mod action;
pub mod blur;
pub mod gif;
pub mod images;
pub mod imeta;
pub mod renderable;

pub use action::{MediaAction, MediaInfo, ViewMediaInfo};
pub use blur::{
    compute_blurhash, update_imeta_blurhashes, ImageMetadata, ObfuscationType, PixelDimensions,
    PointDimensions,
};
use egui::{ColorImage, TextureHandle};
pub use images::ImageType;
pub use renderable::RenderableMedia;

#[derive(Copy, Clone, Debug)]
pub enum AnimationMode {
    /// Only render when scrolling, network activity, etc
    Reactive,

    /// Continuous with an optional target fps
    Continuous { fps: Option<f32> },

    /// Disable animation
    NoAnimation,
}

impl AnimationMode {
    pub fn can_animate(&self) -> bool {
        !matches!(self, Self::NoAnimation)
    }
}

// max size wgpu can handle without panicing
pub const MAX_SIZE_WGPU: usize = 8192;

pub fn load_texture_checked(
    ctx: &egui::Context,
    name: impl Into<String>,
    image: ColorImage,
    options: egui::TextureOptions,
) -> TextureHandle {
    let size = image.size;

    if size[0] > MAX_SIZE_WGPU || size[1] > MAX_SIZE_WGPU {
        panic!("The image MUST be less than or equal to {MAX_SIZE_WGPU} pixels in each direction");
    }

    ctx.load_texture(name, image, options)
}
