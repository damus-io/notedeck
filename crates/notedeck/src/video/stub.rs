#[derive(Default)]
pub struct VideoStore;

impl VideoStore {
    pub fn new() -> Self {
        Self
    }

    pub fn set_fullscreen(&self, _url: &str, _value: bool) {}

    pub fn is_fullscreen(&self, _url: &str) -> bool {
        false
    }

    pub fn remove_player(&self, _url: &str) {}
}
