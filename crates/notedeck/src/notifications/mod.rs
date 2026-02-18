mod backend;
mod bolt11;
mod desktop;
mod extraction;
mod types;

pub use backend::{LoggingBackend, NoopBackend, NotificationBackend};
pub use desktop::DesktopBackend;
pub use types::{
    is_notification_kind, CachedProfile, ExtractedEvent, NotificationAccount, NotificationData,
    WorkerState, NOTIFICATION_KINDS,
};

/// Re-export extraction for use in tests
pub use extraction::extract_event;
