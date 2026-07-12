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

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
