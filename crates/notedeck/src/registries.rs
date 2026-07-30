//! A single home for the read-only, app-contributed registries handed to every
//! frame through [`AppContext`](crate::AppContext).
//!
//! Apps populate these — [`kind_renderers`](AppRegistries::kind_renderers) at
//! startup, [`tools`](AppRegistries::tools) per-frame from the running app set —
//! and the browser looks entries up while drawing. Grouping them here keeps
//! `AppContext` from sprawling one field per registry as more registries are
//! added.
//!
//! The whole struct is exposed as a single shared `&AppRegistries`, so a caller
//! can copy that reference out before taking a disjoint `&mut self` reborrow
//! (e.g. [`note_context`](crate::AppContext::note_context) /
//! [`tool_context`](crate::AppContext::tool_context)) — the same borrow trick
//! each individual registry relied on before they shared a home.

use crate::{kind_renderer::KindRendererRegistry, tool::ToolRegistry};

/// The read-only registries an [`AppContext`](crate::AppContext) exposes to a
/// frame, gathered into one shared handle. See the [module docs](self).
#[derive(Default)]
pub struct AppRegistries {
    /// Renderers for nostr events embedded inline, keyed by kind (e.g. a headway
    /// issue referenced from a notebook note). Populated at app startup.
    pub kind_renderers: KindRendererRegistry,
    /// App-contributed agent tools for AI backends. Reset per-frame from the
    /// running apps.
    pub tools: ToolRegistry,
}
