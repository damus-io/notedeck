use crate::{
    account::accounts::Accounts, frame_history::FrameHistory, i18n::Localization,
    wallet::GlobalWallet, zaps::Zaps, Args, DataPath, Images, JobPool, NoteCache, SettingsHandler,
    UnknownIds,
};
use egui_winit::clipboard::Clipboard;

use enostr::RelayPool;
use nostrdb::Ndb;

#[cfg(target_os = "android")]
use android_activity::AndroidApp;
use egui::{Pos2, Rect};
// TODO: make this interface more sandboxed

pub struct AppContext<'a> {
    pub ndb: &'a mut Ndb,
    pub img_cache: &'a mut Images,
    pub unknown_ids: &'a mut UnknownIds,
    pub pool: &'a mut RelayPool,
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
    pub i18n: &'a mut Localization,

    #[cfg(target_os = "android")]
    pub android: AndroidApp,
}

#[derive(Debug, Clone)]
pub enum SoftKeyboardContext {
    Virtual,
    Platform { ppp: f32 },
}

impl<'a> AppContext<'a> {
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
                    let inset = self.android.get_window_insets(InsetType::Ime);
                    let height = inset.bottom as f32 / ppp;
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
