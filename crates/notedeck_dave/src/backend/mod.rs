pub mod claude;
mod codex;
mod codex_protocol;
mod openai;
mod remote;
mod session_info;
pub(crate) mod shared;
mod task_tracker;
mod tool_summary;
pub mod traits;

pub(crate) use tool_summary::truncate_output;

pub use claude::ClaudeBackend;
pub use codex::CodexBackend;
pub use openai::OpenAiBackend;
pub use remote::RemoteOnlyBackend;
pub use traits::{AiBackend, BackendType, Model};

use agentium_core::Waker;

/// Build an engine [`Waker`] that repaints the desktop egui UI.
///
/// This is the single seam where the egui-ful desktop app adapts to the
/// egui-free engine boundary: backends only ever see a `Waker`, and this turns
/// a `request_repaint` into one.
pub fn egui_waker(ctx: &egui::Context) -> Waker {
    let ctx = ctx.clone();
    Waker::new(move || ctx.request_repaint())
}
