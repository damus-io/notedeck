use std::sync::mpsc::Sender;

use egui::TextureHandle;

use crate::jobs::JobCache;
use crate::{Animation, Error, TextureState, TexturesCache};

use crate::jobs::types::{JobComplete, JobId, JobPackage};

pub type MediaJobs = JobCache<MediaJobKind, MediaJobResult>;
pub type MediaJobSender = Sender<JobPackage<MediaJobKind, MediaJobResult>>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum MediaJobKind {
    Blurhash,
    StaticImg,
    AnimatedImg,
    WebpImg,
}

pub enum MediaJobResult {
    StaticImg(Result<TextureHandle, Error>),
    Blurhash(Result<TextureHandle, Error>),
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
        MediaJobResult::Blurhash(texture_handle) => {
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
        MediaJobKind::Blurhash => {
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
        MediaJobKind::WebpImg => {
            tex_cache.webp.cache.insert(id, TextureState::Pending);
        }
    }
}
