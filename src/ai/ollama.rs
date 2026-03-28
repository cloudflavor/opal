use super::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

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
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

pub fn analyze(
    request: &AiRequest,
    on_chunk: &mut dyn FnMut(AiChunk),
) -> Result<AiResult, AiError> {
    let host = request.host.as_deref().ok_or_else(|| AiError {
        message: "internal error: missing Ollama host".to_string(),
    })?;
    let model = request.model.as_deref().ok_or_else(|| AiError {
        message: "internal error: missing Ollama model".to_string(),
    })?;
    let system = request.system.as_deref();

    let url = format!("{}/api/generate", host.trim_end_matches('/'));
    let body = serde_json::to_string(&OllamaGenerateRequest {
        model,
        prompt: request.prompt.as_str(),
        system,
        stream: true,
    })
    .map_err(|err| AiError {
        message: format!("failed to encode Ollama request: {err}"),
    })?;

    let mut child = Command::new("curl")
        .arg("-sS")
        .arg("-N")
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg(url)
        .arg("--data-binary")
        .arg("@-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to start curl for Ollama")
        .map_err(|err| AiError {
            message: err.to_string(),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body.as_bytes()).map_err(|err| AiError {
            message: format!("failed to write Ollama request: {err}"),
        })?;
    }

    let stdout = child.stdout.take().ok_or_else(|| AiError {
        message: "missing Ollama stdout stream".to_string(),
    })?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut text = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|err| AiError {
            message: format!("failed to read Ollama stream: {err}"),
        })?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let chunk: OllamaGenerateChunk = serde_json::from_str(trimmed).map_err(|err| AiError {
            message: format!("failed to parse Ollama stream chunk: {err}"),
        })?;
        if let Some(error) = chunk.error {
            return Err(AiError { message: error });
        }
        if !chunk.response.is_empty() {
            text.push_str(&chunk.response);
            on_chunk(AiChunk::Text(chunk.response));
        }
        if chunk.done {
            break;
        }
    }

    let output = child.wait_with_output().map_err(|err| AiError {
        message: format!("failed to wait for Ollama process: {err}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AiError {
            message: if stderr.is_empty() {
                format!(
                    "Ollama request failed with status {:?}",
                    output.status.code()
                )
            } else {
                stderr
            },
        });
    }

    Ok(AiResult {
        provider: AiProviderKind::Ollama,
        text,
        saved_path: request.save_path.clone(),
    })
}
