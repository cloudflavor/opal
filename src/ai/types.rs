use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProviderKind {
    Ollama,
    Claude,
    Codex,
}

#[derive(Debug, Clone)]
pub struct AiRequest {
    pub provider: AiProviderKind,
    pub prompt: String,
    pub system: Option<String>,
    pub host: Option<String>,
    pub model: Option<String>,
    pub save_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum AiChunk {
    Text(String),
}

#[derive(Debug, Clone)]
pub struct AiResult {
    pub provider: AiProviderKind,
    pub text: String,
    pub saved_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AiError {
    pub message: String,
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AiError {}
