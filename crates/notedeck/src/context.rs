use crate::{Accounts, Args, DataPath, MediaCache, NoteCache, ThemeHandler, UnknownIds, UrlMimes};

use enostr::RelayPool;
use nostrdb::Ndb;

// TODO: make this interface more sandboxed

pub struct AppContext<'a> {
    pub ndb: &'a mut Ndb,
    pub img_cache: &'a mut MediaCache,
    pub urls: &'a mut UrlMimes,
    pub unknown_ids: &'a mut UnknownIds,
    pub pool: &'a mut RelayPool,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a mut Accounts,
    pub path: &'a DataPath,
    pub args: &'a Args,
    pub theme: &'a mut ThemeHandler,
}
