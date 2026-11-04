//! What a new session runs on: a backend, the backend profile layered on it,
//! and the model it starts on.

use super::{BackendType, Model};

/// A backend picked for a new session, as the backend picker, a clone or a
/// remote spawn names it. Carried whole from the pick to the session (or the
/// kind-31989 spawn command), so the profile can't be dropped on the way and
/// send a profile session's work to the plain backend's account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendChoice {
    /// The backend the session runs on.
    pub backend: BackendType,
    /// The backend profile (kind-31992 slug) layered on it, if any. Only ever
    /// set with [`BackendType::Claude`].
    pub profile: Option<String>,
    /// The model the session starts on.
    pub model: Model,
}

impl BackendChoice {
    /// `backend` with no profile, on the backend's default model.
    pub fn plain(backend: BackendType) -> Self {
        Self {
            backend,
            profile: None,
            model: Model::Default,
        }
    }
}
