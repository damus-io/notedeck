use std::time::{Duration, Instant};

/// A simple debouncer that tracks when an action was last performed
/// and determines if enough time has passed to perform it again.
#[derive(Debug)]
pub struct Debouncer {
    delay: Duration,
    last_action: Instant,
}

impl Debouncer {
    /// Creates a new Debouncer with the specified delay
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            last_action: Instant::now() - delay, // Start ready to act
        }
    }

    /// Sets a new delay value and returns self for method chaining
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Checks if enough time has passed since the last action
    pub fn should_act(&self) -> bool {
        self.last_action.elapsed() >= self.delay
    }

    /// Marks an action as performed, updating the timestamp
    pub fn bounce(&mut self) {
        self.last_action = Instant::now();
    }
}
