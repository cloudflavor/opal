use super::ollama;
use super::{AiChunk, AiError, AiRequest, AiResult};
use super::{claude, codex};

pub async fn analyze_with_default_provider<F>(
    request: &AiRequest,
    on_chunk: F,
) -> Result<AiResult, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    match request.provider {
        super::AiProviderKind::Ollama => ollama::analyze(request, on_chunk).await,
        super::AiProviderKind::Claude => claude::analyze(request, on_chunk).await,
        super::AiProviderKind::Codex => codex::analyze(request, on_chunk).await,
    }
}
