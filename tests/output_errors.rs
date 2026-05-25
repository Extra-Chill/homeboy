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
fn unsupported_runner_validation_error_writes_json_output_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_path = dir.path().join("runner-unsupported.json");

    let output = Command::new(homeboy_bin())
        .args(["--runner", "lab", "--output"])
        .arg(&output_path)
        .arg("status")
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

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
