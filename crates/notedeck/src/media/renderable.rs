use super::ObfuscationType;
use crate::MediaCacheType;

/// Media that is prepared for rendering. Use [`Images::get_renderable_media`] to get these
pub struct RenderableMedia {
    pub url: String,
    pub media_type: MediaCacheType,
    pub obfuscation_type: ObfuscationType,
}
