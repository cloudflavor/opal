use super::ollama;
use super::{AiChunk, AiError, AiRequest, AiResult};
use super::{claude, codex};

pub fn analyze_with_default_provider(
    request: &AiRequest,
    mut on_chunk: impl FnMut(AiChunk),
) -> Result<AiResult, AiError> {
    match request.provider {
        super::AiProviderKind::Ollama => ollama::analyze(request, &mut on_chunk),
        super::AiProviderKind::Claude => claude::analyze(request, &mut on_chunk),
        super::AiProviderKind::Codex => codex::analyze(request, &mut on_chunk),
    }
}
