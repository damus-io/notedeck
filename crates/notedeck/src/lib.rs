#![deny(clippy::disallowed_methods)]

pub mod abbrev;
mod account;
mod app;
mod args;
pub mod contacts;
mod context;
pub mod debouncer;
mod error;
pub mod filter;
pub mod fonts;
mod frame_history;
pub mod i18n;
mod imgcache;
pub mod jobs;
pub mod media;
mod muted;
pub mod name;
pub mod nav;
mod nip51_set;
pub mod note;
mod notecache;
mod options;
mod persist;
pub mod platform;
pub mod profile;
pub mod relay_debug;
pub mod relayspec;
mod result;
mod route;
mod setup;
pub mod storage;
mod style;
pub mod theme;
mod time;
mod timecache;
mod timed_serializer;
pub mod tor;
pub mod ui;
mod unknowns;
mod urls;
mod user_account;
mod wallet;
mod zaps;

pub use account::accounts::{AccountData, AccountSubs, Accounts};
pub use account::contacts::{ContactState, IsFollowing};
pub use account::relay::RelayAction;
pub use account::FALLBACK_PUBKEY;
pub use app::{try_process_events_core, App, AppAction, AppResponse, Notedeck};
pub use args::Args;
pub use context::{AppContext, SoftKeyboardContext};
pub use error::{show_one_error_message, Error, FilterError, ZapError};
pub use filter::{FilterState, FilterStates, UnifiedSubscription};
pub use fonts::NamedFontFamily;
pub use i18n::{CacheStats, FluentArgs, FluentValue, LanguageIdentifier, Localization};
pub use imgcache::{
    Animation, GifState, GifStateMap, ImageFrame, Images, LatestTexture, MediaCache,
    MediaCacheType, TextureFrame, TextureState, TexturesCache,
};
pub use jobs::{
    deliver_completed_media_job, run_media_job_pre_action, JobCache, JobPool, MediaJobSender,
    MediaJobs,
};
pub use media::{
    update_imeta_blurhashes, ImageMetadata, ImageType, MediaAction, ObfuscationType,
    PixelDimensions, PointDimensions, RenderableMedia,
};
pub use muted::{MuteFun, Muted};
pub use name::NostrName;
pub use nav::DragResponse;
pub use nip51_set::{create_nip51_set, Nip51Set, Nip51SetCache};
pub use note::{
    get_p_tags, BroadcastContext, ContextSelection, NoteAction, NoteContext, NoteContextSelection,
    NoteRef, RootIdError, RootNoteId, RootNoteIdBuf, ScrollInfo, ZapAction,
};
pub use notecache::{CachedNote, NoteCache};
pub use options::NotedeckOptions;
pub use persist::*;
pub use profile::*;
pub use relay_debug::RelayDebugView;
pub use relayspec::RelaySpec;
pub use result::Result;
pub use route::{DrawerRouter, ReplacementType, Router};
pub use storage::{AccountStorage, DataPath, DataPathType, Directory};
pub use style::NotedeckTextStyle;
pub use theme::ColorTheme;
pub use time::{
    is_future_timestamp, time_ago_since, time_format, unix_time_secs, MAX_FUTURE_NOTE_SKEW_SECS,
};
pub use timecache::TimeCached;
pub use tor::{TorManager, TorStatus};
pub use unknowns::{get_unknown_note_ids, NoteRefsUnkIdAction, SingleUnkIdAction, UnknownIds};
pub use urls::{supported_mime_hosted_at_url, SupportedMimeType, UrlMimes};
pub use user_account::UserAccount;
pub use wallet::{
    get_current_wallet, get_current_wallet_mut, get_wallet_for, GlobalWallet, Wallet, WalletError,
    WalletType, WalletUIState, ZapWallet,
};
pub use zaps::{
    get_current_default_msats, AnyZapState, DefaultZapError, DefaultZapMsats, NoteZapTarget,
    NoteZapTargetOwned, PendingDefaultZapState, ZapTarget, ZapTargetOwned, ZappingError,
};

// export libs
pub use enostr;
pub use nostrdb;

pub use zaps::Zaps;
