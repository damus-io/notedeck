use crate::{
    account::accounts::Accounts, frame_history::FrameHistory, i18n::Localization,
    wallet::GlobalWallet, zaps::Zaps, Args, DataPath, Images, JobPool, NoteCache, SettingsHandler,
    UnknownIds,
};
use egui_winit::clipboard::Clipboard;

use enostr::RelayPool;
use nostrdb::Ndb;

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
    pub settings_handler: &'a mut SettingsHandler,
    pub clipboard: &'a mut Clipboard,
    pub zaps: &'a mut Zaps,
    pub frame_history: &'a mut FrameHistory,
    pub job_pool: &'a mut JobPool,
    pub i18n: &'a mut Localization,
}
