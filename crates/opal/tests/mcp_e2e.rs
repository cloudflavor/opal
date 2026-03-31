use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

    send_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test-client\",\"version\":\"1.0.0\"}}}",
    );
    let initialize = read_line(&mut reader);
    assert!(initialize.contains("\"name\":\"opal\""));
    assert!(initialize.contains("\"tools\""));

    send_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}",
    );

    send_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}",
    );
    let tools = read_line(&mut reader);
    assert!(tools.contains("opal_plan"));
    assert!(tools.contains("opal_run"));
    assert!(tools.contains("opal_view"));

    send_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"shutdown\",\"params\":{}}",
    );
    let shutdown = read_line(&mut reader);
    assert!(shutdown.contains("\"result\":null"));

    send_line(&mut stdin, "{\"jsonrpc\":\"2.0\",\"method\":\"exit\"}");

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

fn read_line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read line");
    line
}
