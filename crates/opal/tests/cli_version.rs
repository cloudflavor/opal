use std::process::Command;

#[test]
fn version_flag_prints_opal_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_opal"))
        .arg("--version")
        .output()
        .expect("run opal --version");

    assert!(output.status.success(), "version flag should succeed");

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    assert_eq!(stdout.trim(), format!("opal {}", env!("CARGO_PKG_VERSION")));
}
