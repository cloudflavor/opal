use super::{AiChunk, AiError, AiRequest, AiResult};

pub async fn analyze<F>(_request: &AiRequest, _on_chunk: F) -> Result<AiResult, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    Err(AiError {
        message: "Claude Code adapter is not implemented yet".to_string(),
    })
}
