use std::sync::mpsc::Sender;

use egui::TextureHandle;

use crate::jobs::JobCache;
use crate::{Animation, Error};

use crate::jobs::types::JobPackage;

pub type MediaJobs = JobCache<MediaJobKind, MediaJobResult>;
pub type MediaJobSender = Sender<JobPackage<MediaJobKind, MediaJobResult>>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum MediaJobKind {
    Blurhash,
    StaticImg,
    AnimatedImg,
}

pub enum MediaJobResult {
    StaticImg(Result<TextureHandle, Error>),
    Blurhash(Result<TextureHandle, Error>),
    Animation(Result<Animation, Error>),
}
