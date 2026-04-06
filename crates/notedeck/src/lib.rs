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
mod relay_limits;
pub mod relayspec;
mod remote_api;
mod result;
mod route;
mod scoped_sub_api;
mod scoped_sub_owners;
mod scoped_sub_state;
mod scoped_subs;
mod setup;
pub mod sound;
pub mod storage;
mod style;
#[cfg(test)]
pub(crate) mod test_util;
pub mod theme;
mod time;
mod timecache;
pub mod timed_serializer;
pub mod tokens;
pub mod ui;
mod unknowns;
#[cfg(feature = "auto-update")]
pub mod updater;
mod urls;
mod user_account;
mod wallet;
mod zaps;

pub use account::accounts::{giftwrap_sub_identity, AccountData, Accounts};
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

/// Create a [`egui_kittest::wgpu::WgpuTestRenderer`] that only uses software
/// rasterizers (e.g. lavapipe / swiftshader). This gives deterministic
/// snapshot output regardless of host GPU hardware.
///
/// Requires `VK_ICD_FILENAMES` to point at the lavapipe ICD JSON before the
/// process starts (the Vulkan loader caches ICDs on first use, so `set_var`
/// from within the process is too late). On NixOS this is handled by
/// shell.nix via `LAVAPIPE_ICD`; use `scripts/snapshot-test` to run.
/// On standard Linux distros, install `mesa-vulkan-drivers` (or equivalent)
/// and the ICD is auto-discovered.
///
/// Panics at adapter selection time if no CPU adapter is available.
#[cfg(feature = "snapshot-testing")]
pub fn software_renderer() -> egui_kittest::wgpu::WgpuTestRenderer {
    use egui_wgpu::wgpu;
    use std::sync::Arc;

    let mut setup = egui_wgpu::WgpuSetupCreateNew::default();

    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU);

    setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
        adapters
            .iter()
            .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
            .cloned()
            .ok_or_else(|| {
                "No CPU adapter found — install a software rasterizer \
                 (e.g. lavapipe/swiftshader) for deterministic snapshots. \
                 On NixOS, run via: scripts/snapshot-test"
                    .to_owned()
            })
    }));

    egui_kittest::wgpu::WgpuTestRenderer::from_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
}

// export libs
pub use enostr;
pub use nostrdb;

pub use sound::{hover_entered, state_entered, SoundEffect, SoundManager};
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
