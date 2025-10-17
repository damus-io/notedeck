use super::ObfuscationType;
use crate::MediaCacheType;

/// Supported media variants that can be embedded inside Notedeck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderableMediaKind {
    Image(MediaCacheType),
    Video(VideoMedia),
}

/// Metadata describing a video asset. This will expand as we add duration,
/// dimensions, poster frames, etc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoMedia {
    pub codec: VideoCodec,
}

impl VideoMedia {
    pub const fn mp4() -> Self {
        Self {
            codec: VideoCodec::Mp4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    Mp4,
}

impl RenderableMediaKind {
    pub const fn as_cache_type(&self) -> Option<MediaCacheType> {
        match self {
            Self::Image(cache_type) => Some(*cache_type),
            Self::Video(_) => None,
        }
    }
}

/// Media that is prepared for rendering. Use [`Images::get_renderable_media`] to get these.
#[derive(Clone)]
pub struct RenderableMedia {
    pub url: String,
    pub kind: RenderableMediaKind,
    pub obfuscation_type: ObfuscationType,
}
