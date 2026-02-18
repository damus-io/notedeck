mod backend;
mod bolt11;
mod desktop;
mod extraction;
mod profiles;
mod types;

pub use backend::{LoggingBackend, NoopBackend, NotificationBackend};
pub use desktop::DesktopBackend;
pub use profiles::{decode_npub, extract_mentioned_pubkeys, resolve_mentions};
pub use types::{
    is_notification_kind, CachedProfile, ExtractedEvent, NotificationAccount, NotificationData,
    WorkerState, NOTIFICATION_KINDS,
};

/// Re-export extraction for use in tests
pub use extraction::extract_event;
