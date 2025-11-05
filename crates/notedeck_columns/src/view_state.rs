use std::collections::HashMap;
use std::time::{Duration, Instant};

use enostr::Pubkey;
use notedeck_ui::nip51_set::Nip51SetUiCache;

use crate::deck_state::DeckState;
use crate::login_manager::AcquireKeyState;
use crate::ui::search::SearchQueryState;
use enostr::ProfileState;
use notedeck_ui::media::MediaViewerState;

/// Various state for views
///
/// TODO(jb55): we likely want to encapsulate these better,
/// or at least document where they are used
#[derive(Default)]
pub struct ViewState {
    pub login: AcquireKeyState,
    pub id_to_deck_state: HashMap<egui::Id, DeckState>,
    pub id_state_map: HashMap<egui::Id, AcquireKeyState>,
    pub id_string_map: HashMap<egui::Id, String>,
    pub searches: HashMap<egui::Id, SearchQueryState>,
    pub pubkey_to_profile_state: HashMap<Pubkey, ProfileState>,

    /// Keeps track of what urls we are actively viewing in the
    /// fullscreen media viewier, as well as any other state we want to
    /// keep track of
    pub media_viewer: MediaViewerState,

    /// Keep track of checkbox state of follow pack onboarding
    pub follow_packs: Nip51SetUiCache,

    /// Ephemeral toast notifications shown over the UI
    pub toasts: Toasts,
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut AcquireKeyState {
        &mut self.login
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct ToastMessage {
    pub id: u64,
    pub message: String,
    pub created_at: Instant,
    pub kind: ToastKind,
}

#[derive(Debug, Default)]
pub struct Toasts {
    entries: Vec<ToastMessage>,
    next_id: u64,
}

impl Toasts {
    const LIFETIME: Duration = Duration::from_secs(4);

    pub fn push(&mut self, message: impl Into<String>, kind: ToastKind) {
        let msg = ToastMessage {
            id: self.next_id,
            message: message.into(),
            created_at: Instant::now(),
            kind,
        };
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.push(msg);
    }

    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.entries
            .retain(|toast| now.duration_since(toast.created_at) < Self::LIFETIME);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[ToastMessage] {
        &self.entries
    }
}
