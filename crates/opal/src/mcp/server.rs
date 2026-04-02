use super::protocol::{FramingMode, read_message, write_message};
use super::resources::{list_resources, read_resource};
use super::tools::{call_tool, list_tools};
use crate::app::OpalApp;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{self, BufReader};

const PROTOCOL_VERSION: &str = "2025-11-25";
const JSONRPC_VERSION: &str = "2.0";

pub async fn serve_stdio() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let app = OpalApp::from_current_dir()?;
    let mut server = McpServer::new(app);
    let mut framing = None;

    while let Some(message) = read_message(&mut reader, &mut framing)? {
        let framing_mode = framing.unwrap_or(FramingMode::NewlineDelimited);
        if let Some(response) = server.handle_message(message).await? {
            write_message(&mut writer, &response, framing_mode)?;
        }
        if server.should_exit() {
            break;
        }
    }

    Ok(())
}

struct McpServer {
    app: OpalApp,
    shutdown_requested: bool,
    exit_requested: bool,
}

impl McpServer {
    fn new(app: OpalApp) -> Self {
        Self {
            app,
            shutdown_requested: false,
            exit_requested: false,
        }
    }

    fn should_exit(&self) -> bool {
        self.exit_requested
    }

    async fn handle_message(&mut self, message: Value) -> Result<Option<Value>> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .context("missing request method")?;

        if method == "exit" {
            self.exit_requested = true;
            return Ok(None);
        }

        let id = message.get("id").cloned();
        let is_notification = id.is_none();

        if self.shutdown_requested && method != "shutdown" {
            return Ok(id.map(|id| error_response(id, -32000, "server is shut down")));
        }

        let result = match method {
            "initialize" => Ok(initialize_result()),
            "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(list_tools()),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                call_tool(&self.app, params).await
            }
            "resources/list" => list_resources(&self.app).await,
            "resources/read" => {
                let uri = message
                    .get("params")
                    .and_then(|value| value.get("uri"))
                    .and_then(Value::as_str)
                    .context("missing resource URI")?;
                read_resource(&self.app, uri).await
            }
            "shutdown" => {
                self.shutdown_requested = true;
                Ok(Value::Null)
            }
            other => anyhow::bail!("unsupported MCP method '{other}'"),
        };

        if is_notification {
            return Ok(None);
        }

        let id = id.expect("id checked above");
        let response = match result {
            Ok(result) => success_response(id, result),
            Err(err) => error_response(id, -32000, &err.to_string()),
        };
        Ok(Some(response))
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {
                "listChanged": false
            },
            "resources": {
                "subscribe": false,
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "opal",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Use the Opal MCP tools to plan runs, explain job planning decisions, start background pipeline runs and reruns, poll their status, inspect recorded history and logs, compare recent runs, search prior logs, list recent runs with status, date, branch, and pipeline-file filters, and quickly identify failed jobs in the latest or a selected run."
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "result": result
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use super::McpServer;
    use crate::app::OpalApp;
    use serde_json::json;

    #[tokio::test]
    async fn initialize_advertises_tools_and_resources() {
        let app = OpalApp::from_current_dir().expect("app");
        let mut server = McpServer::new(app);
        let response = server
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "test",
                        "version": "1.0.0"
                    }
                }
            }))
            .await
            .expect("initialize")
            .expect("response");

        assert!(response["result"]["capabilities"].get("tools").is_some());
        assert!(
            response["result"]["capabilities"]
                .get("resources")
                .is_some()
        );
    }
}
