use egui_video::{AudioDevice, Player};
use poll_promise::Promise;
use sdl2;
use std::cell::RefCell;
use std::collections::HashMap;

/// Stores per-URL video playback state for platforms that support embedded video.
#[derive(Default)]
pub struct VideoStore {
    players: RefCell<HashMap<String, VideoSlot>>,
    fullscreen: RefCell<HashMap<String, bool>>,
    audio: RefCell<Option<AudioSupport>>,
}

impl VideoStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_players<R>(&self, f: impl FnOnce(&mut HashMap<String, VideoSlot>) -> R) -> R {
        let mut players = self.players.borrow_mut();
        f(&mut players)
    }

    pub fn with_fullscreen<R>(&self, f: impl FnOnce(&mut HashMap<String, bool>) -> R) -> R {
        let mut fullscreen = self.fullscreen.borrow_mut();
        f(&mut fullscreen)
    }

    pub fn with_audio<R>(&self, f: impl FnOnce(&mut Option<AudioSupport>) -> R) -> R {
        let mut audio = self.audio.borrow_mut();
        f(&mut audio)
    }

    pub fn is_fullscreen(&self, url: &str) -> bool {
        self.fullscreen.borrow().get(url).copied().unwrap_or(false)
    }

    pub fn set_fullscreen(&self, url: &str, value: bool) {
        self.with_fullscreen(|fullscreen| {
            if value {
                fullscreen.insert(url.to_owned(), true);
            } else {
                fullscreen.remove(url);
            }
        });
    }

    pub fn remove_player(&self, url: &str) {
        self.with_players(|players| {
            players.remove(url);
        });
    }
}

pub enum VideoSlot {
    Loading {
        promise: Promise<Result<String, String>>,
    },
    Ready {
        player: Player,
        started: bool,
    },
    Failed(String),
}

pub struct AudioSupport {
    pub sdl: sdl2::Sdl,
    pub device: AudioDevice,
}
