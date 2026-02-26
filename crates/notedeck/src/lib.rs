#![deny(clippy::disallowed_methods)]

pub mod abbrev;
mod account;
mod app;
mod args;
pub mod async_loader;
pub mod compact;
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
pub mod nip05;
mod nip51_set;
pub mod note;
mod notecache;
mod oneshot_api;
mod options;
mod persist;
pub mod platform;
pub mod profile;
mod publish;
pub mod relay_debug;
pub mod relayspec;
mod remote_api;
mod result;
mod route;
mod scoped_sub_api;
mod scoped_sub_owners;
mod scoped_sub_state;
mod scoped_subs;
mod setup;
pub mod storage;
mod style;
pub mod theme;
mod time;
mod timecache;
pub mod timed_serializer;
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
pub use app::{App, AppAction, AppResponse, Notedeck};
pub use args::Args;
pub use async_loader::{worker_count, AsyncLoader};
pub use context::{AppContext, SoftKeyboardContext};
use enostr::{OutboxSessionHandler, Wakeup};
pub use error::{show_one_error_message, Error, FilterError, ZapError};
pub use filter::{FilterState, UnifiedSubscription};
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
pub use nip05::{Nip05Cache, Nip05Status};
pub use nip51_set::{create_nip51_set, Nip51Set, Nip51SetCache};
pub use note::{
    builder_from_note, get_p_tags, send_mute_event, send_people_list_event, send_report_event,
    send_unmute_event, BroadcastContext, ContextSelection, NoteAction, NoteContext,
    NoteContextSelection, NoteRef, ReportTarget, ReportType, RootIdError, RootNoteId,
    RootNoteIdBuf, ScrollInfo, ZapAction,
};
pub use notecache::{CachedNote, NoteCache};
pub use oneshot_api::OneshotApi;
pub use options::NotedeckOptions;
pub use persist::*;
pub use profile::*;
pub use publish::{AccountsPublishApi, ExplicitPublishApi, PublishApi, RelayType};
pub use relay_debug::RelayDebugView;
pub use relayspec::RelaySpec;
pub use remote_api::{RelayInspectApi, RelayInspectEntry, RemoteApi};
pub use result::Result;
pub use route::{DrawerRouter, ReplacementType, Router};
pub use scoped_sub_api::ScopedSubApi;
pub use scoped_sub_owners::SubOwnerKeyBuilder;
pub use scoped_sub_state::ScopedSubsState;
pub use scoped_subs::{
    ClearSubResult, DropSlotResult, EnsureSubResult, RelaySelection, ScopedSubEoseStatus,
    ScopedSubIdentity, ScopedSubLiveEoseStatus, SetSubResult, SubConfig, SubKey, SubKeyBuilder,
    SubOwnerKey, SubScope,
};
pub use storage::{AccountStorage, DataPath, DataPathType, Directory};
pub use style::NotedeckTextStyle;
pub use theme::ColorTheme;
pub use time::{
    is_future_timestamp, time_ago_since, time_format, unix_time_secs, MAX_FUTURE_NOTE_SKEW_SECS,
};
pub use timecache::TimeCached;
pub use unknowns::{
    get_unknown_note_ids, unknown_id_send, NoteRefsUnkIdAction, SingleUnkIdAction, UnknownIds,
};
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

pub type Outbox<'a> = OutboxSessionHandler<'a, EguiWakeup>;

#[derive(Clone)]
pub struct EguiWakeup(egui::Context);

impl EguiWakeup {
    pub fn new(ctx: egui::Context) -> Self {
        Self(ctx)
    }
}

impl Wakeup for EguiWakeup {
    fn wake(&self) {
        self.0.request_repaint();
    }
}
