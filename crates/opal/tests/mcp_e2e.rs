use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn opal_mcp_subcommand_supports_initialize_and_tools() {
    let temp = temp_test_dir("opal-mcp-subcommand");
    fs::write(
        temp.join(".gitlab-ci.yml"),
        "stages:\n  - test\n\nhello:\n  stage: test\n  script:\n    - echo hello\n",
    )
    .expect("write pipeline");
    let opal_home = temp.join("opal-home");
    fs::create_dir_all(&opal_home).expect("opal home");

    let mut child = Command::new(env!("CARGO_BIN_EXE_opal"))
        .arg("mcp")
        .current_dir(&temp)
        .env("OPAL_HOME", &opal_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn opal mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            }
        }),
    );
    let initialize = parse_line(&mut reader);
    assert_eq!(initialize["result"]["serverInfo"]["name"], "opal");
    assert!(initialize["result"]["capabilities"].get("tools").is_some());

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    );
    let tools = parse_line(&mut reader);
    let tool_names = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"opal_plan"));
    assert!(tool_names.contains(&"opal_run"));
    assert!(tool_names.contains(&"opal_run_status"));
    assert!(tool_names.contains(&"opal_view"));

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": {}
        }),
    );
    let shutdown = parse_line(&mut reader);
    assert_eq!(shutdown["result"], Value::Null);

    send_json(&mut stdin, json!({"jsonrpc": "2.0", "method": "exit"}));

    let status = child.wait().expect("wait child");
    assert!(status.success());
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn opal_mcp_subcommand_supports_resources_and_background_run_status() {
    let temp = temp_test_dir("opal-mcp-resources-run-status");
    fs::write(
        temp.join(".gitlab-ci.yml"),
        "stages:\n  - test\n\nhello:\n  stage: test\n  script:\n    - echo hello\n",
    )
    .expect("write pipeline");
    let opal_home = temp.join("opal-home");
    fs::create_dir_all(&opal_home).expect("opal home");

    let mut child = Command::new(env!("CARGO_BIN_EXE_opal"))
        .arg("mcp")
        .current_dir(&temp)
        .env("OPAL_HOME", &opal_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn opal mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            }
        }),
    );
    let _ = parse_line(&mut reader);

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/list",
            "params": {}
        }),
    );
    let resources = parse_line(&mut reader);
    let uris = resources["result"]["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|entry| entry["uri"].as_str())
        .collect::<Vec<_>>();
    assert!(uris.contains(&"opal://history"));

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {
                "uri": "opal://history"
            }
        }),
    );
    let history = parse_line(&mut reader);
    assert_eq!(
        history["result"]["contents"][0]["mimeType"],
        "application/json"
    );

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "opal_run",
                "arguments": {
                    "pipeline": "missing.yml"
                }
            }
        }),
    );
    let start = parse_line(&mut reader);
    assert_eq!(start["result"]["isError"], false);
    let operation_id = start["result"]["structuredContent"]["operation"]["operation_id"]
        .as_str()
        .expect("operation id")
        .to_string();

    let mut terminal = None;
    for request_id in 5..45 {
        send_json(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {
                    "name": "opal_run_status",
                    "arguments": {
                        "operation_id": operation_id
                    }
                }
            }),
        );
        let status = parse_line(&mut reader);
        let state = status["result"]["structuredContent"]["operation"]["status"]
            .as_str()
            .expect("status");
        if state != "running" {
            terminal = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let terminal = terminal.expect("terminal status");
    assert_eq!(
        terminal["result"]["structuredContent"]["operation"]["status"],
        "failed"
    );
    assert_eq!(
        terminal["result"]["structuredContent"]["operation"]["run"],
        Value::Null
    );
    assert!(
        !terminal["result"]["structuredContent"]["operation"]["error"]
            .as_str()
            .expect("error")
            .is_empty()
    );

    send_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 50,
            "method": "shutdown",
            "params": {}
        }),
    );
    let shutdown = parse_line(&mut reader);
    assert_eq!(shutdown["result"], Value::Null);

    send_json(&mut stdin, json!({"jsonrpc": "2.0", "method": "exit"}));

    let status = child.wait().expect("wait child");
    assert!(status.success());
    let _ = fs::remove_dir_all(temp);
}

fn temp_test_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn send_line(stdin: &mut impl Write, line: &str) {
    stdin.write_all(line.as_bytes()).expect("write line");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush stdin");
}

fn send_json(stdin: &mut impl Write, value: Value) {
    send_line(
        stdin,
        &serde_json::to_string(&value).expect("serialize json"),
    );
}

fn read_line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read line");
    line
}

fn parse_line(reader: &mut impl BufRead) -> Value {
    serde_json::from_str(&read_line(reader)).expect("json line")
}
