use super::shared::{ai_error, contextual_error, emit_text_chunk, missing_internal};
use super::{AiChunk, AiError, AiProviderKind, AiRequest, AiResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

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
    let command = request
        .command
        .as_deref()
        .ok_or_else(|| missing_internal("Codex command"))?;
    let output_path = temp_output_file();
    let mut child = spawn_codex(command, build_args(request, &output_path))?;
    write_prompt(child.stdin.take(), &request.prompt).await?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| missing_internal("Codex stdout stream"))?;
    let streamed = stream_codex_output(stdout, &mut on_chunk).await?;

    let output = child
        .wait_with_output()
        .await
        .map_err(|err| contextual_error("failed to wait for Codex CLI", err))?;
    ensure_success(&output)?;

    let final_text = read_final_text(&output_path, &streamed).await;
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

fn build_args(request: &AiRequest, output_path: &Path) -> Vec<String> {
    if request.args.is_empty() {
        default_args(
            request.workdir.as_deref(),
            output_path,
            request.model.as_deref(),
        )
    } else {
        request.args.clone()
    }
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

fn spawn_codex(command: &str, args: Vec<String>) -> Result<Child, AiError> {
    Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| contextual_error("failed to start Codex CLI", err))
}

async fn write_prompt(
    stdin: Option<tokio::process::ChildStdin>,
    prompt: &str,
) -> Result<(), AiError> {
    let Some(mut stdin) = stdin else {
        return Ok(());
    };

    stdin
        .write_all(prompt.as_bytes())
        .await
        .map_err(|err| contextual_error("failed to write prompt to Codex CLI", err))?;
    stdin
        .flush()
        .await
        .map_err(|err| contextual_error("failed to write prompt to Codex CLI", err))
}

async fn stream_codex_output<F>(stdout: ChildStdout, on_chunk: &mut F) -> Result<String, AiError>
where
    F: FnMut(AiChunk) + Send,
{
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut streamed = String::new();

    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|err| contextual_error("failed to read Codex output", err))?;
        if read == 0 {
            break;
        }

        if let Some(delta) = parse_event_delta(line.trim()) {
            emit_text_chunk(&mut streamed, delta, on_chunk);
        }
    }

    Ok(streamed)
}

fn parse_event_delta(line: &str) -> Option<String> {
    if line.is_empty() {
        return None;
    }

    let event = serde_json::from_str::<CodexJsonEvent>(line).ok()?;
    if event.method.as_deref() != Some("agent/messageDelta") {
        return None;
    }

    event.params.and_then(|params| {
        let delta = params.delta?;
        (!delta.is_empty()).then_some(delta)
    })
}

fn ensure_success(output: &std::process::Output) -> Result<(), AiError> {
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        ai_error(format!(
            "Codex CLI failed with status {:?}",
            output.status.code()
        ))
    } else {
        ai_error(stderr)
    })
}

async fn read_final_text(output_path: &Path, streamed: &str) -> String {
    match fs::read_to_string(output_path).await {
        Ok(text) => text,
        Err(_) => streamed.to_string(),
    }
}

fn temp_output_file() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    std::env::temp_dir().join(format!("opal-codex-last-message-{pid}-{nanos}.txt"))
}

#[cfg(test)]
mod tests {
    use super::{CodexJsonEvent, parse_event_delta};

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

    #[test]
    fn parse_event_delta_ignores_non_delta_events() {
        assert_eq!(
            parse_event_delta(r#"{"method":"other","params":{"delta":"hello"}}"#),
            None
        );
        assert_eq!(parse_event_delta(""), None);
    }
}
