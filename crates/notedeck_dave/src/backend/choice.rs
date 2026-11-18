//! What a new session runs on: a backend, the backend profile layered on it,
//! the agent spec it runs under, and the model it starts on.

use super::{BackendType, Model};

/// A backend picked for a new session, as the backend picker, a clone or a
/// remote spawn names it. Carried whole from the pick to the session (or the
/// kind-31989 spawn command), so the profile can't be dropped on the way and
/// send a profile session's work to the plain backend's account, and the spec
/// can't be dropped and leave a session on a backend with none of its tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendChoice {
    /// The backend the session runs on.
    pub backend: BackendType,
    /// The backend profile (kind-31992 slug) layered on it, if any. Only ever
    /// set with [`BackendType::Claude`].
    pub profile: Option<String>,
    /// The agent spec (its slug) the session runs under, if any. Only ever
    /// set with [`BackendType::OpenAI`]: a spec's turns run in the native
    /// loop on the spec's own model, whatever the choice's `model` says.
    pub spec: Option<String>,
    /// The model the session starts on.
    pub model: Model,
}

impl BackendChoice {
    /// `backend` with no profile, on the backend's default model.
    pub fn plain(backend: BackendType) -> Self {
        Self {
            backend,
            profile: None,
            spec: None,
            model: Model::Default,
        }
    }

    /// A session under the agent spec `slug`: the native loop
    /// ([`BackendType::OpenAI`]), on the spec's own model.
    pub fn under_spec(slug: impl Into<String>) -> Self {
        Self {
            spec: Some(slug.into()),
            ..Self::plain(BackendType::OpenAI)
        }
    }
}
