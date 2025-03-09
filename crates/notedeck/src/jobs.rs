use egui::{ahash::HashMap, TextureHandle};
use poll_promise::Promise;

#[derive(Default)]
pub struct Jobs {
    pub jobs: HashMap<JobId, Promise<Job>>,
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub enum JobId {
    Blurhash(String),
}

pub enum Job {
    ProcessBlurhash(Option<TextureHandle>),
}
