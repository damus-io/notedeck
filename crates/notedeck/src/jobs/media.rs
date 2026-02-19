use std::sync::mpsc::Sender;

use egui::TextureHandle;

use crate::jobs::JobCache;
use crate::media::images::TextureRequestKey;
use crate::{Animation, Error, TextureState, TexturesCache};

use crate::jobs::types::{JobComplete, JobId, JobPackage};

pub type MediaJobs = JobCache<MediaJobKind, MediaJobResult>;
pub type MediaJobSender = Sender<JobPackage<MediaJobKind, MediaJobResult>>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum MediaJobKind {
    Blurhash,
    StaticImg { request_key: TextureRequestKey },
    AnimatedImg { request_key: TextureRequestKey },
}

pub enum MediaJobResult {
    StaticImg(Result<TextureHandle, Error>),
    Blurhash(Result<TextureHandle, Error>),
    Animation(Result<Animation, Error>),
}

/// Converts a job result into the shared texture cache state shape.
fn into_texture_state<T>(result: Result<T, Error>) -> TextureState<T> {
    match result {
        Ok(value) => TextureState::Loaded(value),
        Err(error) => TextureState::Error(error),
    }
}

pub fn deliver_completed_media_job(
    completed: JobComplete<MediaJobKind, MediaJobResult>,
    tex_cache: &mut TexturesCache,
) {
    let JobComplete { job_id, response } = completed;
    let id = job_id.id;
    let id_c = id.clone();
    match (job_id.job_kind, response) {
        (MediaJobKind::StaticImg { request_key }, MediaJobResult::StaticImg(job_complete)) => {
            tex_cache
                .static_image
                .cache
                .insert(request_key, into_texture_state(job_complete));
        }
        (MediaJobKind::AnimatedImg { request_key }, MediaJobResult::Animation(animation)) => {
            tex_cache
                .animated
                .cache
                .insert(request_key, into_texture_state(animation));
        }
        (MediaJobKind::Blurhash, MediaJobResult::Blurhash(texture_handle)) => {
            tex_cache
                .blurred
                .cache
                .insert(id, into_texture_state(texture_handle).into());
        }
        (job_kind, _) => {
            tracing::error!(
                "mismatched media job completion kind for id {id_c}: {:?}",
                job_kind
            );
        }
    }
    tracing::trace!("Delivered job for {id_c}");
}

pub fn run_media_job_pre_action(job_id: &JobId<MediaJobKind>, tex_cache: &mut TexturesCache) {
    let id = job_id.id.clone();
    match &job_id.job_kind {
        MediaJobKind::Blurhash => {
            tex_cache
                .blurred
                .cache
                .insert(id, TextureState::Pending.into());
        }
        MediaJobKind::StaticImg { request_key } => {
            tex_cache
                .static_image
                .cache
                .insert(request_key.clone(), TextureState::Pending);
        }
        MediaJobKind::AnimatedImg { request_key } => {
            tex_cache
                .animated
                .cache
                .insert(request_key.clone(), TextureState::Pending);
        }
    }
}
