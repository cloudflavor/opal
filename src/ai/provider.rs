use super::ollama;
use super::{AiChunk, AiError, AiRequest, AiResult};

pub fn analyze_with_default_provider(
    request: &AiRequest,
    mut on_chunk: impl FnMut(AiChunk),
) -> Result<AiResult, AiError> {
    match request.provider {
        super::AiProviderKind::Ollama => ollama::analyze(request, &mut on_chunk),
        super::AiProviderKind::Claude => Err(AiError {
            message: "Claude Code adapter is not implemented yet".to_string(),
        }),
        super::AiProviderKind::Codex => Err(AiError {
            message: "Codex adapter is not implemented yet".to_string(),
        }),
    }
}
