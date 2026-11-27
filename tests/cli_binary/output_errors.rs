use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn validation_error_writes_json_output_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_path = dir.path().join("runner-unsupported.json");
    register_local_runner(dir.path());

    let output = homeboy_command()
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

    let output_file_bytes = std::fs::read(&output_path).expect("read output file");
    assert_eq!(output.stdout, output_file_bytes);
    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    let file_json: Value = serde_json::from_slice(&output_file_bytes).expect("output file json");

    assert_eq!(file_json, stdout_json);
    assert_eq!(file_json["schema"], "homeboy/command-result/v3");
    assert_eq!(file_json["command"], "runner");
    assert_eq!(file_json["operation"], "exec");
    assert_eq!(file_json["success"], false);
    assert_eq!(
        file_json["diagnostics"]["code"],
        "validation.invalid_argument"
    );
    assert!(file_json.get("data").is_none());
}

#[test]
fn human_flag_prints_the_summary_and_keeps_json_in_the_output_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_path = dir.path().join("runner-unsupported.json");
    register_local_runner(dir.path());

    let output = homeboy_command()
        .args(["--human", "--output"])
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
    let file_json: Value =
        serde_json::from_slice(&std::fs::read(&output_path).expect("read output file"))
            .expect("output file json");
    let summary = file_json["summary"].as_str().expect("envelope summary");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        summary.trim_end()
    );
}

#[test]
fn artifact_destination_is_not_rejected_as_global_format() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = homeboy_command()
        .args([
            "runs",
            "artifact",
            "get",
            "missing-run",
            "missing-artifact",
            "--to",
            "json",
        ])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run homeboy");

    assert!(!output.status.success());

    let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
    let message = stdout_json["diagnostics"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !message.contains("looks like an output format"),
        "the artifact destination should not be validated as the global envelope path: {message}"
    );
}

/// Compact and full runner status both separate local placement from a Lab
/// connection; only the full view projects the selected Lab runner.
#[test]
fn local_runner_status_separates_placement_from_lab_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    register_lab_runner(dir.path());

    for full in [false, true] {
        let mut command = homeboy_command();
        command.args(["runner", "status", "lab-local"]);
        if full {
            command.arg("--full");
        }
        let output = command
            .current_dir(dir.path())
            .env("HOME", dir.path())
            .output()
            .expect("run homeboy");

        assert!(
            output.status.success(),
            "runner status (full={full}) should succeed; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout_json: Value = serde_json::from_slice(&output.stdout).expect("stdout json");
        let data = &stdout_json["data"];
        assert_eq!(data["command"], "runner.status");
        assert_eq!(data["id"], "lab-local");
        assert_eq!(
            data["execution_capabilities"]["local_placement"]["available"],
            true
        );
        assert_eq!(
            data["execution_capabilities"]["lab_runner_connection"]["available"],
            false
        );
        if full {
            assert_eq!(data["selected_lab_runner"]["connected"], false);
            assert_eq!(
                data["selected_lab_runner"]["availability"]["connected"],
                false
            );
        } else {
            assert!(data.get("selected_lab_runner").is_none());
        }
    }
}

#[test]
fn explicit_json_path_is_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    register_local_runner(dir.path());

    let output = homeboy_command()
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
    assert_eq!(
        file_json["diagnostics"]["code"],
        "validation.invalid_argument"
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy_command() -> Command {
    let mut command = Command::new(homeboy_bin());
    command.env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}

fn register_local_runner(home: &std::path::Path) {
    let output = homeboy_command()
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

fn register_lab_runner(home: &std::path::Path) {
    let server = homeboy_command()
        .args([
            "server",
            "create",
            "lab-local",
            "--host",
            "localhost",
            "--user",
            "test",
        ])
        .env("HOME", home)
        .output()
        .expect("register Lab server");
    assert!(
        server.status.success(),
        "expected Lab server registration to succeed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&server.stdout),
        String::from_utf8_lossy(&server.stderr)
    );

    let runner = homeboy_command()
        .args([
            "runner",
            "add",
            "lab-local",
            "--kind",
            "ssh",
            "--server",
            "lab-local",
            "--workspace-root",
        ])
        .arg(home)
        .env("HOME", home)
        .output()
        .expect("register Lab runner");
    assert!(
        runner.status.success(),
        "expected Lab runner registration to succeed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&runner.stdout),
        String::from_utf8_lossy(&runner.stderr)
    );
}
