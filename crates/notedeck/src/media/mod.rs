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
