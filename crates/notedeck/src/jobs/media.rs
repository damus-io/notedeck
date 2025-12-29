use std::sync::mpsc::Sender;

use egui::TextureHandle;

use crate::jobs::JobCache;
use crate::{Animation, Error, TextureState, TexturesCache};

use crate::jobs::types::{JobComplete, JobId, JobPackage};

pub type MediaJobs = JobCache<MediaJobKind, MediaJobResult>;
pub type MediaJobSender = Sender<JobPackage<MediaJobKind, MediaJobResult>>;

/// The type of media job being processed.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum MediaJobKind {
    /// Decode a blurhash string into a placeholder texture
    Blurhash,
    /// Decode a thumbhash into a placeholder texture
    ThumbHash,
    /// Load a static image
    StaticImg,
    /// Load an animated image (GIF, etc.)
    AnimatedImg,
}

/// The result of a completed media job.
pub enum MediaJobResult {
    StaticImg(Result<TextureHandle, Error>),
    Blurhash(Result<TextureHandle, Error>),
    ThumbHash(Result<TextureHandle, Error>),
    Animation(Result<Animation, Error>),
}

pub fn deliver_completed_media_job(
    completed: JobComplete<MediaJobKind, MediaJobResult>,
    tex_cache: &mut TexturesCache,
) {
    let id = completed.job_id.id;
    let id_c = id.clone();
    match completed.response {
        MediaJobResult::StaticImg(job_complete) => {
            let r = match job_complete {
                Ok(t) => TextureState::Loaded(t),
                Err(e) => TextureState::Error(e),
            };
            tex_cache.static_image.cache.insert(id, r);
        }
        MediaJobResult::Animation(animation) => {
            let r = match animation {
                Ok(a) => TextureState::Loaded(a),
                Err(e) => TextureState::Error(e),
            };
            tex_cache.animated.cache.insert(id, r);
        }
        // Both blurhash and thumbhash results go to the blur cache
        MediaJobResult::Blurhash(texture_handle) | MediaJobResult::ThumbHash(texture_handle) => {
            let r = match texture_handle {
                Ok(t) => TextureState::Loaded(t),
                Err(e) => TextureState::Error(e),
            };
            tex_cache.blurred.cache.insert(id, r.into());
        }
    }
    tracing::trace!("Delivered job for {id_c}");
}

pub fn run_media_job_pre_action(job_id: &JobId<MediaJobKind>, tex_cache: &mut TexturesCache) {
    let id = job_id.id.clone();
    match job_id.job_kind {
        // Both blurhash and thumbhash use the blur cache
        MediaJobKind::Blurhash | MediaJobKind::ThumbHash => {
            tex_cache
                .blurred
                .cache
                .insert(id, TextureState::Pending.into());
        }
        MediaJobKind::StaticImg => {
            tex_cache
                .static_image
                .cache
                .insert(id, TextureState::Pending);
        }
        MediaJobKind::AnimatedImg => {
            tex_cache.animated.cache.insert(id, TextureState::Pending);
        }
    }
}
