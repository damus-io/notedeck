use super::context::ContextSelection;
use crate::{zaps::NoteZapTargetOwned, Images, MediaCacheType, TexturedImage};
use enostr::{NoteId, Pubkey};
use poll_promise::Promise;

#[derive(Debug)]
pub enum NoteAction {
    /// User has clicked the quote reply action
    Reply(NoteId),

    /// User has clicked the quote repost action
    Quote(NoteId),

    /// User has clicked a hashtag
    Hashtag(String),

    /// User has clicked a profile
    Profile(Pubkey),

    /// User has clicked a note link
    Note(NoteId),

    /// User has selected some context option
    Context(ContextSelection),

    /// User has clicked the zap action
    Zap(ZapAction),

    /// User clicked on media
    Media(MediaAction),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ZapAction {
    Send(ZapTargetAmount),
    ClearError(NoteZapTargetOwned),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ZapTargetAmount {
    pub target: NoteZapTargetOwned,
    pub specified_msats: Option<u64>, // if None use default amount
}

pub enum MediaAction {
    FetchImage {
        url: String,
        cache_type: MediaCacheType,
        no_pfp_promise: Promise<Option<Result<TexturedImage, crate::Error>>>,
    },
    DoneLoading {
        url: String,
        cache_type: MediaCacheType,
    },
}

impl std::fmt::Debug for MediaAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchImage {
                url,
                cache_type,
                no_pfp_promise,
            } => f
                .debug_struct("FetchNoPfpImage")
                .field("url", url)
                .field("cache_type", cache_type)
                .field("no_pfp_promise ready", &no_pfp_promise.ready().is_some())
                .finish(),
            Self::DoneLoading { url, cache_type } => f
                .debug_struct("DoneLoading")
                .field("url", url)
                .field("cache_type", cache_type)
                .finish(),
        }
    }
}

impl MediaAction {
    pub fn process(self, images: &mut Images) {
        match self {
            MediaAction::FetchImage {
                url,
                cache_type,
                no_pfp_promise: promise,
            } => {
                images
                    .get_cache_mut(cache_type)
                    .textures_cache
                    .insert_pending(&url, promise);
            }
            MediaAction::DoneLoading { url, cache_type } => {
                let cache = match cache_type {
                    MediaCacheType::Image => &mut images.static_imgs,
                    MediaCacheType::Gif => &mut images.gifs,
                };

                cache.textures_cache.move_to_loaded(&url);
            }
        }
    }
}
