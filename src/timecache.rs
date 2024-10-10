use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct TimeCached<T> {
    last_update: Instant,
    expires_in: Duration,
    value: Option<T>,
    refresh: Arc<dyn Fn() -> T + Send + 'static>,
}

impl<T> TimeCached<T> {
    pub fn new(expires_in: Duration, refresh: impl Fn() -> T + Send + 'static) -> Self {
        TimeCached {
            last_update: Instant::now(),
            expires_in,
            value: None,
            refresh: Arc::new(refresh),
        }
    }

    pub fn needs_update(&self) -> bool {
        self.value.is_none() || self.last_update.elapsed() > self.expires_in
    }

    pub fn update(&mut self) {
        self.last_update = Instant::now();
        self.value = Some((self.refresh)());
    }

    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn get_mut(&mut self) -> &T {
        if self.needs_update() {
            self.update();
        }
        self.value.as_ref().unwrap() // This unwrap is safe because we just set the value if it was None.
    }
}
