use homeboy::core::error::{RemoteCommandFailedDetails, TargetDetails};
use homeboy::core::Error;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn remote_command_failed_creates_error_with_details() {
    let err = Error::remote_command_failed(RemoteCommandFailedDetails {
        command: "ls -la".to_string(),
        exit_code: 127,
        stdout: "some stdout".to_string(),
        stderr: "some stderr".to_string(),
        target: TargetDetails {
            project_id: Some("alpha".to_string()),
            server_id: Some("server1".to_string()),
            host: Some("example.com".to_string()),
        },
    });

    assert_eq!(err.code.as_str(), "remote.command_failed");
    assert_eq!(err.message, "Remote command failed");
    // Command details are in the serialized details, not the message
    let details_str = err.details.to_string();
    assert!(details_str.contains("ls -la"));
    assert!(details_str.contains("some stdout"));
    assert!(details_str.contains("some stderr"));
}

#[test]
fn validation_error_writes_json_output_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_path = dir.path().join("runner-unsupported.json");
    register_local_runner(dir.path());

    let output = Command::new(homeboy_bin())
        .args(["--output"])
        .arg(&output_path)
        .args([
            "runner",
            "exec",
            "lab-local",
            "--require-path",
            "relative-path",
            "true",
        ])
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output_path.exists(),
        "expected --output file to be written; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    let file_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&output_path).expect("read output file"))
            .expect("output file json");

    assert_eq!(file_json, stdout_json);
    assert_eq!(file_json["success"], false);
    assert_eq!(file_json["error"]["code"], "validation.invalid_argument");
    assert!(file_json.get("data").is_none());
}

#[test]
fn output_json_is_rejected_as_format_footgun() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = Command::new(homeboy_bin())
        .args(["--runner", "lab", "--output", "json", "status"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        !dir.path().join("json").exists(),
        "bare --output json should not create a literal json file"
    );

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    assert_eq!(stdout_json["success"], false);
    assert_eq!(stdout_json["error"]["code"], "validation.invalid_argument");
    assert!(stdout_json["error"]["message"]
        .as_str()
        .expect("message")
        .contains("looks like an output format"));
}

#[test]
fn output_equals_json_is_rejected_as_format_footgun() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = Command::new(homeboy_bin())
        .args(["--runner", "lab", "--output=json", "status"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        !dir.path().join("json").exists(),
        "bare --output=json should not create a literal json file"
    );

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    assert_eq!(stdout_json["error"]["code"], "validation.invalid_argument");
}

#[test]
fn command_owned_output_path_is_not_rejected_as_global_format() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = Command::new(homeboy_bin())
        .args([
            "runs",
            "artifact",
            "get",
            "missing-run",
            "missing-artifact",
            "--output",
            "json",
        ])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert!(!output.status.success());

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    let message = stdout_json["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("looks like an output format"),
        "command-owned --output should not be validated as the global envelope path: {message}"
    );
}

#[test]
fn lab_status_accepts_command_owned_runner_selector() {
    let dir = tempfile::tempdir().expect("tempdir");
    register_local_runner(dir.path());

    let output = Command::new(homeboy_bin())
        .args(["lab", "status", "--runner", "lab-local"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert!(
        output.status.success(),
        "lab status --runner should succeed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    assert_eq!(stdout_json["success"], true);
    assert_eq!(stdout_json["data"]["command"], "lab.status");
    assert_eq!(
        stdout_json["data"]["selected_runner"]["runner_id"],
        "lab-local"
    );
    assert_eq!(
        stdout_json["data"]["selected_runner"]["configured_executable"],
        "homeboy"
    );
    assert_eq!(
        stdout_json["data"]["selected_runner"]["workspace_root"],
        dir.path().to_string_lossy().as_ref()
    );
    assert_eq!(
        stdout_json["data"]["selected_runner"]["readiness_state"],
        "disconnected"
    );
    assert_eq!(
        stdout_json["data"]["selected_runner"]["status"]["state"],
        "disconnected"
    );
}

#[test]
fn explicit_json_path_is_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    register_local_runner(dir.path());

    let output = Command::new(homeboy_bin())
        .args([
            "--output",
            "./json",
            "runner",
            "exec",
            "lab-local",
            "--require-path",
            "relative-path",
            "true",
        ])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert_eq!(output.status.code(), Some(2));

    let output_path = dir.path().join("json");
    assert!(
        output_path.exists(),
        "explicit relative path should still be accepted; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let file_json: Value =
        serde_json::from_str(&std::fs::read_to_string(output_path).expect("read output file"))
            .expect("output file json");
    assert_eq!(file_json["error"]["code"], "validation.invalid_argument");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn register_local_runner(home: &std::path::Path) {
    let output = Command::new(homeboy_bin())
        .args([
            "runner",
            "add",
            "lab-local",
            "--kind",
            "local",
            "--workspace-root",
        ])
        .arg(home)
        .env("HOME", home)
        .output()
        .expect("register local runner");

    assert!(
        output.status.success(),
        "expected local runner registration to succeed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
