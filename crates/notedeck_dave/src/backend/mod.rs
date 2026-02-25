pub mod claude;
mod codex;
mod codex_protocol;
mod openai;
mod remote;
mod session_info;
pub(crate) mod shared;
mod tool_summary;
mod traits;

pub use claude::ClaudeBackend;
pub use codex::CodexBackend;
pub use openai::OpenAiBackend;
pub use remote::RemoteOnlyBackend;
pub use traits::{AiBackend, BackendType};
