use super::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Deserialize)]
struct CodexJsonEvent {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<CodexEventParams>,
}

#[derive(Debug, Deserialize)]
struct CodexEventParams {
    #[serde(default)]
    delta: Option<String>,
}

pub async fn analyze<F>(request: &AiRequest, mut on_chunk: F) -> Result<AiResult, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    let command = request.command.as_deref().ok_or_else(|| AiError {
        message: "internal error: missing Codex command".to_string(),
    })?;
    let output_path = temp_output_file();

    let mut args = if request.args.is_empty() {
        default_args(
            request.workdir.as_deref(),
            &output_path,
            request.model.as_deref(),
        )
    } else {
        request.args.clone()
    };

    let mut child = Command::new(command)
        .args(args.drain(..))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| AiError {
            message: format!("failed to start Codex CLI: {err}"),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .map_err(|err| AiError {
                message: format!("failed to write prompt to Codex CLI: {err}"),
            })?;
        stdin.flush().await.map_err(|err| AiError {
            message: format!("failed to write prompt to Codex CLI: {err}"),
        })?;
    }

    let stdout = child.stdout.take().ok_or_else(|| AiError {
        message: "missing Codex stdout stream".to_string(),
    })?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut streamed = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line).await.map_err(|err| AiError {
            message: format!("failed to read Codex output: {err}"),
        })?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<CodexJsonEvent>(trimmed)
            && event.method.as_deref() == Some("agent/messageDelta")
            && let Some(delta) = event.params.and_then(|params| params.delta)
            && !delta.is_empty()
        {
            streamed.push_str(&delta);
            on_chunk(AiChunk::Text(delta));
        }
    }

    let output = child.wait_with_output().await.map_err(|err| AiError {
        message: format!("failed to wait for Codex CLI: {err}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AiError {
            message: if stderr.is_empty() {
                format!("Codex CLI failed with status {:?}", output.status.code())
            } else {
                stderr
            },
        });
    }

    let final_text = fs::read_to_string(&output_path)
        .await
        .unwrap_or_else(|_| streamed.clone());
    let _ = fs::remove_file(&output_path).await;

    Ok(AiResult {
        provider: AiProviderKind::Codex,
        text: if final_text.is_empty() {
            streamed
        } else {
            final_text
        },
        saved_path: request.save_path.clone(),
    })
}

fn default_args(workdir: Option<&Path>, output_path: &Path, model: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-a".to_string(),
        "never".to_string(),
        "-s".to_string(),
        "read-only".to_string(),
        "exec".to_string(),
        "--json".to_string(),
        "--output-last-message".to_string(),
        output_path.display().to_string(),
    ];
    if let Some(workdir) = workdir {
        args.push("-C".to_string());
        args.push(workdir.display().to_string());
    }
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args.push("-".to_string());
    args
}

fn temp_output_file() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("opal-codex-last-message-{nanos}.txt"))
}

#[cfg(test)]
mod tests {
    use super::CodexJsonEvent;

    #[test]
    fn parses_agent_message_delta_event() {
        let event: CodexJsonEvent =
            serde_json::from_str(r#"{"method":"agent/messageDelta","params":{"delta":"hello"}}"#)
                .expect("parse event");
        assert_eq!(event.method.as_deref(), Some("agent/messageDelta"));
        assert_eq!(
            event.params.and_then(|params| params.delta).as_deref(),
            Some("hello")
        );
    }
}
