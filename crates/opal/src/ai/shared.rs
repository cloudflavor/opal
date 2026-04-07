use super::{AiChunk, AiError};
use std::fmt::Display;

pub(crate) fn ai_error(message: impl Into<String>) -> AiError {
    AiError {
        message: message.into(),
    }
}

pub(crate) fn missing_internal(field: &str) -> AiError {
    ai_error(format!("internal error: missing {field}"))
}

pub(crate) fn contextual_error(context: &str, err: impl Display) -> AiError {
    ai_error(format!("{context}: {err}"))
}

pub(crate) fn emit_text_chunk<F>(buffer: &mut String, chunk: impl Into<String>, on_chunk: &mut F)
where
    F: FnMut(AiChunk),
{
    let chunk = chunk.into();
    if chunk.is_empty() {
        return;
    }
    buffer.push_str(&chunk);
    on_chunk(AiChunk::Text(chunk));
}

#[cfg(test)]
mod tests {
    use super::emit_text_chunk;
    use crate::ai::AiChunk;

    #[test]
    fn emit_text_chunk_appends_and_streams_non_empty_text() {
        let mut buffer = String::new();
        let mut chunks = Vec::new();

        emit_text_chunk(&mut buffer, "hello", &mut |chunk| match chunk {
            AiChunk::Text(text) => chunks.push(text),
        });
        emit_text_chunk(&mut buffer, "", &mut |_| {});

        assert_eq!(buffer, "hello");
        assert_eq!(chunks, vec!["hello".to_string()]);
    }
}
