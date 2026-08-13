use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

#[test]
fn promotion_provider_reads_its_request_from_process_stdin() {
    let mut child = Command::new(homeboy_bin())
        .args([
            "agent-task",
            "promotion-provider",
            "--workspace",
            "/unused-for-invalid-request",
        ])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn promotion provider");

    child
        .stdin
        .take()
        .expect("promotion provider stdin")
        .write_all(b"{}")
        .expect("write promotion provider request");

    let output = child
        .wait_with_output()
        .expect("wait for promotion provider");
    let response: Value = serde_json::from_slice(&output.stdout).expect("provider response JSON");

    assert!(!output.status.success());
    assert_eq!(
        response["diagnostics"]["details"]["context"],
        "agent-task promotion provider request"
    );
    assert_ne!(response["diagnostics"]["details"]["category"], "eof");
}
