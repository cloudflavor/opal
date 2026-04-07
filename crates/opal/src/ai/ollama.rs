use super::shared::{ai_error, contextual_error, emit_text_chunk, missing_internal};
use super::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
use reqwest::header::ACCEPT;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::mem;
use std::time::Duration;

#[derive(Debug, Serialize)]
struct OllamaGenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaGenerateChunk {
    #[serde(default)]
    response: String,
    #[serde(default)]
    error: Option<String>,
}

pub async fn analyze<F>(request: &AiRequest, on_chunk: F) -> Result<AiResult, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    let host = request
        .host
        .as_deref()
        .ok_or_else(|| missing_internal("Ollama host"))?;
    let model = request
        .model
        .as_deref()
        .ok_or_else(|| missing_internal("Ollama model"))?;
    let system = request.system.as_deref();

    let url = generate_url(host);
    let payload = OllamaGenerateRequest {
        model,
        prompt: request.prompt.as_str(),
        system,
        stream: true,
    };

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| contextual_error("failed to build Ollama HTTP client", err))?;

    let response = client
        .post(&url)
        .header(ACCEPT, "application/x-ndjson")
        .json(&payload)
        .send()
        .await
        .map_err(|err| contextual_error(&format!("failed to call Ollama at {url}"), err))?;

    let text = parse_generate_response(response, on_chunk).await?;

    Ok(AiResult {
        provider: AiProviderKind::Ollama,
        text,
        saved_path: request.save_path.clone(),
    })
}

fn generate_url(host: &str) -> String {
    format!("{}/api/generate", host.trim_end_matches('/'))
}

async fn parse_generate_response<F>(
    mut response: Response,
    mut on_chunk: F,
) -> Result<String, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    let status = response.status();
    if !status.is_success() {
        let body: String = response.text().await.unwrap_or_default();
        return Err(ollama_status_error(status, &body));
    }
    let mut text = String::new();
    let mut buffer = Vec::new();
    loop {
        let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| contextual_error("failed to read Ollama stream", err))?
        else {
            break;
        };
        buffer.extend_from_slice(&chunk);
        drain_ndjson_lines(&mut buffer, &mut text, &mut on_chunk)?;
    }
    if !buffer.is_empty() {
        let tail = mem::take(&mut buffer);
        parse_stream_line(&tail, &mut text, &mut on_chunk)?;
    }
    Ok(text)
}

#[cfg(test)]
fn parse_generate_stream(
    body: &[u8],
    mut on_chunk: impl FnMut(AiChunk),
) -> Result<String, AiError> {
    let mut text = String::new();
    let mut buffer = body.to_vec();
    drain_ndjson_lines(&mut buffer, &mut text, &mut on_chunk)?;
    if !buffer.is_empty() {
        let tail = mem::take(&mut buffer);
        parse_stream_line(&tail, &mut text, &mut on_chunk)?;
    }
    Ok(text)
}

fn drain_ndjson_lines<F>(
    buffer: &mut Vec<u8>,
    text: &mut String,
    on_chunk: &mut F,
) -> Result<(), AiError>
where
    F: FnMut(AiChunk),
{
    while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
        let line = buffer.drain(..=pos).collect::<Vec<_>>();
        parse_stream_line(&line, text, on_chunk)?;
    }
    Ok(())
}

fn parse_stream_line<F>(line: &[u8], text: &mut String, on_chunk: &mut F) -> Result<(), AiError>
where
    F: FnMut(AiChunk),
{
    let trimmed = std::str::from_utf8(line)
        .map_err(|err| contextual_error("failed to decode Ollama stream chunk", err))?
        .trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let chunk: OllamaGenerateChunk = serde_json::from_str(trimmed)
        .map_err(|err| contextual_error("failed to parse Ollama stream chunk", err))?;
    if let Some(error) = chunk.error {
        return Err(ai_error(error));
    }
    emit_text_chunk(text, chunk.response, on_chunk);
    Ok(())
}

fn ollama_status_error(status: reqwest::StatusCode, body: &str) -> AiError {
    let message = if body.trim().is_empty() {
        format!("Ollama request failed with status {status}")
    } else {
        format!(
            "Ollama request failed with status {status}: {}",
            body.trim()
        )
    };
    ai_error(message)
}

#[cfg(test)]
mod tests {
    use super::{AiChunk, OllamaGenerateRequest, generate_url, parse_generate_stream};

    #[test]
    fn generate_url_respects_base_path() {
        assert_eq!(
            generate_url("http://127.0.0.1:11434/prefix"),
            "http://127.0.0.1:11434/prefix/api/generate"
        );
    }

    #[test]
    fn generate_request_body_uses_documented_streaming_shape() {
        let body = serde_json::to_value(&OllamaGenerateRequest {
            model: "llama3",
            prompt: "hello",
            system: Some("sys"),
            stream: true,
        })
        .expect("encode body");
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["prompt"], "hello");
        assert_eq!(body["system"], "sys");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn parse_generate_streams_generate_chunks_without_curl() {
        let mut chunks = Vec::new();
        let result = parse_generate_stream(
            concat!(
                "{\"response\":\"hello\",\"done\":false}\n",
                "{\"response\":\" world\",\"done\":true}\n"
            )
            .as_bytes(),
            &mut |chunk| match chunk {
                AiChunk::Text(text) => chunks.push(text),
            },
        )
        .expect("parse stream");

        assert_eq!(result, "hello world");
        assert_eq!(chunks, vec!["hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn parse_generate_stream_surfaces_stream_errors() {
        let err = parse_generate_stream(
            concat!(
                "{\"response\":\"partial\",\"done\":false}\n",
                "{\"error\":\"model exploded\"}\n"
            )
            .as_bytes(),
            &mut |_| {},
        )
        .expect_err("stream should error");
        assert_eq!(err.message, "model exploded");
    }
}
