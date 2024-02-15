use std::time::{Duration, Instant};

pub struct TimeCached<T> {
    last_update: Instant,
    expires_in: Duration,
    value: Option<T>,
    refresh: Box<dyn Fn() -> T + 'static>,
}

impl<T> TimeCached<T> {
    pub fn new(expires_in: Duration, refresh: Box<dyn Fn() -> T + 'static>) -> Self {
        TimeCached {
            last_update: Instant::now(),
            expires_in,
            value: None,
            refresh,
        }
    }

    pub fn get(&mut self) -> &T {
        if self.value.is_none() || self.last_update.elapsed() > self.expires_in {
            self.last_update = Instant::now();
            self.value = Some((self.refresh)());
        }
        self.value.as_ref().unwrap() // This unwrap is safe because we just set the value if it was None.
    }
}
