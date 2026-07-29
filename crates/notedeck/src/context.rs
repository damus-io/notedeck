use crate::{
    account::accounts::Accounts, frame_history::FrameHistory, i18n::Localization,
    nip05::Nip05Cache, sound::SoundManager, wallet::GlobalWallet, zaps::ZapVerifier, zaps::Zaps,
    Args, DataPath, Images, JobPool, MediaJobs, NoteCache, RemoteApi, SettingsHandler, UnknownIds,
};
use egui_winit::clipboard::Clipboard;
use enostr::Pubkey;

use nostrdb::{Ndb, Transaction};

#[cfg(target_os = "android")]
use android_activity::AndroidApp;
use egui::{Pos2, Rect};
// TODO: make this interface more sandboxed

pub struct AppContext<'a> {
    pub ndb: &'a mut Ndb,
    pub img_cache: &'a mut Images,
    pub unknown_ids: &'a mut UnknownIds,
    /// Relay/outbox transport APIs (scoped subs, oneshot, publish, relay inspect).
    pub remote: RemoteApi<'a>,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a mut Accounts,
    pub global_wallet: &'a mut GlobalWallet,
    pub path: &'a DataPath,
    pub args: &'a Args,
    pub settings: &'a mut SettingsHandler,
    pub clipboard: &'a mut Clipboard,
    pub zaps: &'a mut Zaps,
    pub zap_verifier: &'a mut ZapVerifier,
    pub frame_history: &'a mut FrameHistory,
    pub job_pool: &'a mut JobPool,
    pub media_jobs: &'a mut MediaJobs,
    pub nip05_cache: &'a mut Nip05Cache,
    pub i18n: &'a mut Localization,
    pub sound: &'a SoundManager,
    /// Renderers for nostr events embedded inline, keyed by kind.
    pub kind_renderers: &'a crate::kind_renderer::KindRendererRegistry,
    /// App-contributed agent tools, for AI backends. Read-only enumeration
    /// coexists with a disjoint [`tool_context`](AppContext::tool_context)
    /// mut-borrow via the same `&'a` trick as `kind_renderers`.
    pub tools: &'a crate::tool::ToolRegistry,
    /// Actions raised imperatively this frame (e.g. by a clicked inline
    /// [`KindRenderer`](crate::KindRenderer) widget) for the shell to route after
    /// `render`. Push via [`AppActionQueue::push`](crate::AppActionQueue::push).
    pub app_actions: &'a mut crate::AppActionQueue,

    #[cfg(target_os = "android")]
    pub android: AndroidApp,
}

#[derive(Debug, Clone)]
pub enum SoftKeyboardContext {
    Virtual,
    Platform { ppp: f32 },
}

impl SoftKeyboardContext {
    pub fn platform(context: &egui::Context) -> Self {
        Self::Platform {
            ppp: context.pixels_per_point(),
        }
    }
}

impl<'a> AppContext<'a> {
    /// Borrow the note-rendering dependencies as a [`NoteContext`], for code
    /// that wants to draw notes with notedeck_ui widgets (e.g. `NoteView`).
    ///
    /// This reborrows the relevant fields of the context, so the returned
    /// `NoteContext` holds a mutable borrow of `self` for its lifetime — drop it
    /// before touching other `AppContext` fields. Fields not part of
    /// `NoteContext` (e.g. `kind_renderers`) should be copied/read out first.
    pub fn note_context(&mut self) -> crate::NoteContext<'_> {
        crate::NoteContext {
            ndb: self.ndb,
            accounts: self.accounts,
            global_wallet: self.global_wallet,
            img_cache: self.img_cache,
            note_cache: self.note_cache,
            zaps: self.zaps,
            jobs: self.media_jobs.sender(),
            unknown_ids: self.unknown_ids,
            nip05_cache: self.nip05_cache,
            clipboard: self.clipboard,
            i18n: self.i18n,
            sound: self.sound,
        }
    }

    /// Borrow the browser state an agent tool executes against as a
    /// [`ToolContext`](crate::ToolContext), for a backend dispatching a
    /// [`ToolCall`](crate::ToolCall). Like [`note_context`](Self::note_context),
    /// this reborrows the relevant fields, so the returned context holds a
    /// mutable borrow of `self` for its lifetime.
    pub fn tool_context(&mut self) -> crate::ToolContext<'_, 'a> {
        crate::ToolContext {
            ndb: self.ndb,
            note_cache: self.note_cache,
            accounts: self.accounts,
            publish: Some(self.remote.publisher_explicit()),
        }
    }

    /// Every agent-tool spec available to a backend: the browser's built-in
    /// [`ToolCall`](crate::ToolCall) set followed by the app-contributed
    /// [`tools`](Self::tools). This is the advertise surface (OpenAI `tools`, MCP
    /// `tools/list`); [`call_tool`](Self::call_tool) is the matching dispatch.
    pub fn tool_specs(&self) -> Vec<crate::ToolSpec> {
        let mut specs = crate::ToolCall::specs();
        specs.extend(self.tools.specs().cloned());
        specs
    }

    /// Dispatch an agent tool by `name` with raw JSON `args`, over a
    /// [`tool_context`](Self::tool_context). Browser
    /// [`ToolCall`](crate::ToolCall)s take precedence over app tools of the same
    /// name; an unknown name yields [`ToolOutcome::Error`](crate::ToolOutcome).
    pub fn call_tool(&mut self, name: &str, args: &serde_json::Value) -> crate::ToolOutcome {
        // Browser tools are privileged and win on a name clash.
        if crate::ToolCall::specs()
            .iter()
            .any(|spec| spec.name() == name)
        {
            return match crate::ToolCall::parse(name, args) {
                Ok(call) => call.call(&mut self.tool_context()),
                Err(err) => crate::ToolOutcome::Error(err),
            };
        }

        // `tools` is a shared `&'a` borrow, so copying the reference out frees
        // `self` to be mut-borrowed by `tool_context()` (same trick as
        // `kind_renderers`).
        let tools = self.tools;
        tools.call(&mut self.tool_context(), name, args)
    }

    pub fn select_account(&mut self, pubkey: &Pubkey) {
        let txn = Transaction::new(self.ndb).expect("txn");
        self.accounts
            .select_account(pubkey, self.ndb, &txn, &mut self.remote);
    }

    pub fn remove_account(&mut self, pubkey: &Pubkey) -> bool {
        self.accounts
            .remove_account(pubkey, self.ndb, &mut self.remote)
    }

    pub fn process_relay_action(&mut self, action: crate::RelayAction) {
        self.accounts.process_relay_action(&mut self.remote, action);
    }

    pub fn soft_keyboard_rect(&self, screen_rect: Rect, ctx: SoftKeyboardContext) -> Option<Rect> {
        match ctx {
            SoftKeyboardContext::Virtual => {
                let height = 400.0;
                skb_rect_from_screen_rect(screen_rect, height)
            }

            #[allow(unused_variables)]
            SoftKeyboardContext::Platform { ppp } => {
                #[cfg(target_os = "android")]
                {
                    use android_activity::InsetType;

                    // not sure why I need this, it seems to be consistently off by some amount of
                    // pixels ?
                    let fudge = 0.0;

                    let inset = self.android.get_window_insets(InsetType::Ime);
                    let height = (inset.bottom as f32 / ppp) - fudge;
                    skb_rect_from_screen_rect(screen_rect, height)
                }

                #[cfg(not(target_os = "android"))]
                {
                    None
                }
            }
        }
    }
}

#[inline]
fn skb_rect_from_screen_rect(screen_rect: Rect, height: f32) -> Option<Rect> {
    if height == 0.0 {
        return None;
    }
    let min = Pos2::new(0.0, screen_rect.max.y - height);
    Some(Rect::from_min_max(min, screen_rect.max))
}
