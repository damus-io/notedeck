use std::collections::HashSet;

use crate::{
    frame_history::FrameHistory, wallet::GlobalWallet, zaps::Zaps, Accounts, Args, DataPath,
    Images, JobPool, NoteCache, ThemeHandler, UnknownIds,
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
    pub theme: &'a mut ThemeHandler,
    pub clipboard: &'a mut Clipboard,
    pub zaps: &'a mut Zaps,
    pub frame_history: &'a mut FrameHistory,
    pub job_pool: &'a mut JobPool,
    pub missing_events_ids: &'a mut HashSet<[u8; 32]>,
}
