use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

pub type GifStateMap = HashMap<String, GifState>;

pub struct GifState {
    last_frame_rendered: Instant,
    last_frame_duration: Duration,
    next_frame_time: Option<SystemTime>,
    last_frame_index: usize,
}
