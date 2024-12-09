use crate::{Accounts, Args, DataPath, Images, NoteCache, SubMan, ThemeHandler, UnknownIds};
use egui_winit::clipboard::Clipboard;

use nostrdb::Ndb;

// TODO: make this interface more sandboxed

pub struct AppContext<'a> {
    pub ndb: &'a mut Ndb,
    pub img_cache: &'a mut Images,
    pub unknown_ids: &'a mut UnknownIds,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a mut Accounts,
    pub path: &'a DataPath,
    pub args: &'a Args,
    pub theme: &'a mut ThemeHandler,
    pub clipboard: &'a mut Clipboard,
    pub subman: &'a mut SubMan,
}
