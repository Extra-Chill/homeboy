use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn shipped_root_binary_reports_root_version_and_source_commit() {
    let expected = homeboy_product_identity::build_identity();
    let output = Command::new(homeboy_bin())
        .args(["self", "identity"])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run shipped root binary identity command");

    assert_eq!(output.status.code(), Some(0));
    let identity: Value = serde_json::from_slice(&output.stdout).expect("identity JSON");
    assert_eq!(identity["data"]["version"], expected.version);
    assert_eq!(
        identity["data"]["git_commit"],
        serde_json::to_value(expected.git_commit.clone()).expect("serialize build commit")
    );
    assert_eq!(
        identity["data"]["git_dirty"],
        serde_json::to_value(expected.git_dirty).expect("serialize dirty marker")
    );
    let expected_binary = homeboy_bin().to_string_lossy().into_owned();
    assert_eq!(
        identity["data"]["active_binary"].as_str(),
        Some(expected_binary.as_str())
    );

    let inspect = Command::new(homeboy_bin())
        .args(["self", "inspect"])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run identity discovery alias");

    assert_eq!(inspect.status.code(), Some(0));
    assert_eq!(inspect.stdout, output.stdout);

    let output = Command::new(homeboy_bin())
        .arg("--version")
        .output()
        .expect("run shipped root binary version fast path");

    assert_eq!(output.status.code(), Some(0));
    let version = String::from_utf8(output.stdout).expect("version output is UTF-8");
    assert!(version.contains(&expected.version));
    if let Some(commit) = &expected.git_commit {
        assert!(version.contains(commit));
    }
    assert_eq!(version.contains("-dirty"), expected.git_dirty == Some(true));
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
