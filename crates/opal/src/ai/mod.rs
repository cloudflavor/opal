mod claude;
mod codex;
mod context;
mod ollama;
mod prompt;
mod provider;
mod types;

pub use context::AiContext;
pub use prompt::{RenderedPrompt, render_job_analysis_prompt};
pub use provider::analyze_with_default_provider;
pub use types::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
