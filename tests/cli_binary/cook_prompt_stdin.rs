use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn cook_rejects_empty_whitespace_padded_prompt_stdin_before_destination_or_provider_preflight() {
    let output = Command::new(homeboy_bin())
        .args([
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--prompt",
            " \t-\n ",
            "--backend",
            "fixture",
            "--no-finalize",
        ])
        .stdin(Stdio::piped())
        .env("HOME", tempfile::tempdir().expect("home").path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy");

    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("agent-task cook --prompt - received empty stdin")
            || stderr.contains("agent-task cook --prompt - received empty stdin"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("provider") && !stderr.contains("provider"),
        "empty stdin must fail before provider preflight\nstdout: {stdout}\nstderr: {stderr}"
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
