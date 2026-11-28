use homeboy::core::observation::{NewRunRecord, ObservationStore};
use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use homeboy_engine_primitives::content_hash::sha256_hex;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn adapter_backed_contract_preserves_stdout_and_output_file_envelopes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_path = dir.path().join("manifest.json");
    let output = Command::new(homeboy_bin())
        .args(["contract", "manifest", "--output"])
        .arg(&output_path)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run contract manifest");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());

    let output_file_bytes = std::fs::read(&output_path).expect("manifest output file");
    assert_eq!(output.stdout, output_file_bytes);
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("stdout JSON");
    let output_file: Value = serde_json::from_slice(&output_file_bytes).expect("output file JSON");
    assert_eq!(stdout, output_file);
    assert_eq!(stdout["success"], true);
    assert_eq!(stdout["data"]["command"], "contract.manifest");
    assert!(stdout["data"]["commands"].is_array());
}

#[test]
fn self_identity_preserves_canonical_envelopes_on_stdout_and_output_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stdout = Command::new(homeboy_bin())
        .args(["self", "identity"])
        .output()
        .expect("run self identity fast path");
    assert_eq!(stdout.status.code(), Some(0));
    assert!(stdout.stderr.is_empty());
    assert_canonical_self_identity_envelope(&stdout.stdout);

    let output_path = dir.path().join("identity.json");
    let output = Command::new(homeboy_bin())
        .args(["self", "identity", "--output"])
        .arg(&output_path)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run self identity with output file");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());

    let output_file = std::fs::read(&output_path).expect("identity output file");
    assert_eq!(output.stdout, output_file);
    assert_canonical_self_identity_envelope(&output.stdout);
}

#[test]
fn artifact_get_handle_keeps_artifact_bytes_separate_from_json_output() {
    let context = HermeticTestContext::new();
    let bytes = b"artifact bytes\0not a command-result envelope\xff";
    let source = context.root().join("source.bin");
    std::fs::write(&source, bytes).expect("write source");
    let store = ObservationStore::open_initialized_in_roots(&context.path_roots()).expect("store");
    let run = store
        .start_run(NewRunRecord::builder("bench").build())
        .expect("run");
    store
        .record_artifact_with_metadata(
            &run.id,
            "failure.diagnostic",
            &source,
            serde_json::json!({"failure_diagnostic": true, "name": "recorded.bin"}),
        )
        .expect("record artifact");
    let handle = store
        .bounded_artifact_projection_for_runs(&[run.id], 1)
        .expect("projection")
        .diagnostic_handle
        .expect("handle");
    drop(store);

    let json_path = context.root().join("result.json");
    for (destination, flags) in [
        ("recorded.bin", &[][..]),
        ("short.bin", &["-o", "short.bin"]),
        ("long.bin", &["--to", "long.bin", "--output", "result.json"]),
    ] {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .current_dir(context.root())
            .args(["runs", "artifact", "get-handle", &handle])
            .args(flags)
            .output()
            .expect("run get-handle");
        assert!(output.status.success(), "{output:?}");
        let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON result");
        assert_eq!(envelope["schema"], "homeboy/command-result/v3");
        assert_eq!(envelope["success"], true);
        let destination = context.root().join(destination);
        let fetched = std::fs::read(&destination).expect("read fetched artifact");
        assert_eq!(fetched, bytes);
        let result = &envelope["data"]["payload"];
        assert_eq!(result["output_path"], destination.to_str().unwrap());
        assert_eq!(result["sha256"], sha256_hex(&fetched));
        if flags.contains(&"--output") {
            assert_eq!(
                std::fs::read(&json_path).expect("JSON output"),
                output.stdout
            );
        } else {
            assert!(!json_path.exists());
        }
    }

    let destination = context.root().join("short.bin");
    std::fs::write(&destination, b"keep this file").expect("seed conflicting destination");
    let output = context
        .command(TestBinary::HomeboyFixture)
        .current_dir(context.root())
        .arg("--output")
        .arg(&destination)
        .args([
            "runs",
            "artifact",
            "get-handle",
            &handle,
            "-o",
            "./short.bin",
        ])
        .output()
        .expect("run get-handle with conflicting paths");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("must name different files"));
    assert_eq!(
        std::fs::read(destination).expect("unchanged destination"),
        b"keep this file"
    );
}

fn assert_canonical_self_identity_envelope(bytes: &[u8]) {
    let envelope: Value = serde_json::from_slice(bytes).expect("self identity JSON envelope");
    assert_eq!(envelope["command"], "self");
    assert_eq!(envelope["operation"], "identity");
    assert_eq!(envelope["success"], true);
    assert_eq!(envelope["status"], "succeeded");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
