use super::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

pub async fn analyze<F>(request: &AiRequest, mut on_chunk: F) -> Result<AiResult, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    let command = request.command.as_deref().ok_or_else(|| AiError {
        message: "internal error: missing Claude command".to_string(),
    })?;

    let mut args = if request.args.is_empty() {
        default_args(request.model.as_deref(), request.system.as_deref())
    } else {
        request.args.clone()
    };

    let mut cmd = Command::new(command);
    cmd.args(args.drain(..))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(workdir) = request.workdir.as_deref() {
        cmd.current_dir(workdir);
    }
    let mut child = cmd.spawn().map_err(|err| AiError {
        message: format!("failed to start Claude Code CLI: {err}"),
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .map_err(|err| AiError {
                message: format!("failed to write prompt to Claude Code CLI: {err}"),
            })?;
        stdin.flush().await.map_err(|err| AiError {
            message: format!("failed to write prompt to Claude Code CLI: {err}"),
        })?;
    }

    let stdout = child.stdout.take().ok_or_else(|| AiError {
        message: "missing Claude Code stdout stream".to_string(),
    })?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut streamed = String::new();
    let mut final_text = None;

    loop {
        line.clear();
        let read = reader.read_line(&mut line).await.map_err(|err| AiError {
            message: format!("failed to read Claude Code output: {err}"),
        })?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(delta) = stream_delta(&value) {
            streamed.push_str(delta);
            on_chunk(AiChunk::Text(delta.to_string()));
        }
        if let Some(text) = final_response_text(&value) {
            final_text = Some(text);
        }
    }

    let output = child.wait_with_output().await.map_err(|err| AiError {
        message: format!("failed to wait for Claude Code CLI: {err}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AiError {
            message: if stderr.is_empty() {
                format!(
                    "Claude Code CLI failed with status {:?}",
                    output.status.code()
                )
            } else {
                stderr
            },
        });
    }

    Ok(AiResult {
        provider: AiProviderKind::Claude,
        text: final_text.unwrap_or(streamed),
        saved_path: request.save_path.clone(),
    })
}

fn default_args(model: Option<&str>, system: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--include-partial-messages".to_string(),
        "--permission-mode".to_string(),
        "plan".to_string(),
    ];
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if let Some(system) = system.filter(|value| !value.trim().is_empty()) {
        args.push("--append-system-prompt".to_string());
        args.push(system.to_string());
    }
    args
}

fn stream_delta(value: &Value) -> Option<&str> {
    if value.get("type").and_then(Value::as_str) != Some("stream_event") {
        return None;
    }
    let delta = value.get("event")?.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    delta.get("text").and_then(Value::as_str)
}

fn final_response_text(value: &Value) -> Option<String> {
    if let Some(result) = value.get("result").and_then(Value::as_str)
        && !result.is_empty()
    {
        return Some(result.to_string());
    }

    if let Some(message) = value.get("message")
        && let Some(text) = message_text(message)
    {
        return Some(text);
    }

    message_text(value)
}

fn message_text(value: &Value) -> Option<String> {
    let content = value.get("content")?.as_array()?;
    let mut blocks = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            continue;
        };
        if !text.is_empty() {
            blocks.push(text);
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::{default_args, final_response_text, stream_delta};
    use serde_json::json;

    #[test]
    fn default_args_match_headless_streaming_shape() {
        let args = default_args(Some("claude-sonnet-4-6"), Some("focus on root causes"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--output-format", "stream-json"])
        );
        assert!(args.iter().any(|arg| arg == "--verbose"));
        assert!(args.iter().any(|arg| arg == "--include-partial-messages"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode", "plan"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "claude-sonnet-4-6"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--append-system-prompt", "focus on root causes"])
        );
    }

    #[test]
    fn stream_delta_reads_text_delta_events() {
        let value = json!({
            "type": "stream_event",
            "event": {
                "delta": {
                    "type": "text_delta",
                    "text": "hello"
                }
            }
        });

        assert_eq!(stream_delta(&value), Some("hello"));
    }

    #[test]
    fn final_response_text_prefers_result_field() {
        let value = json!({
            "type": "result",
            "result": "final answer"
        });

        assert_eq!(final_response_text(&value).as_deref(), Some("final answer"));
    }

    #[test]
    fn final_response_text_reads_message_content() {
        let value = json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "first" },
                    { "type": "tool_use", "name": "Bash" },
                    { "type": "text", "text": "second" }
                ]
            }
        });

        assert_eq!(
            final_response_text(&value).as_deref(),
            Some("first\n\nsecond")
        );
    }
}
