use crate::{
    account::accounts::Accounts, frame_history::FrameHistory, i18n::Localization,
    nip05::Nip05Cache, wallet::GlobalWallet, zaps::Zaps, Args, DataPath, Images, JobPool,
    MediaJobs, NoteCache, RemoteApi, SettingsHandler, UnknownIds,
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
    pub frame_history: &'a mut FrameHistory,
    pub job_pool: &'a mut JobPool,
    pub media_jobs: &'a mut MediaJobs,
    pub nip05_cache: &'a mut Nip05Cache,
    pub i18n: &'a mut Localization,

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
