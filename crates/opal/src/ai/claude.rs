use super::{AiChunk, AiError, AiRequest, AiResult};

pub fn analyze(
    _request: &AiRequest,
    _on_chunk: &mut dyn FnMut(AiChunk),
) -> Result<AiResult, AiError> {
    Err(AiError {
        message: "Claude Code adapter is not implemented yet".to_string(),
    })
}
