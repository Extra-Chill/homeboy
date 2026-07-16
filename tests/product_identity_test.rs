use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn shipped_root_binary_reports_root_version_and_source_commit() {
    let expected_version = env!("CARGO_PKG_VERSION");
    let expected_commit = git_output(&["rev-parse", "--short=12", "HEAD"]);
    let expected_dirty = !git_output(&["status", "--porcelain"]).is_empty();
    let output = Command::new(homeboy_bin())
        .args(["self", "identity"])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run shipped root binary identity command");

    assert_eq!(output.status.code(), Some(0));
    let identity: Value = serde_json::from_slice(&output.stdout).expect("identity JSON");
    assert_eq!(identity["data"]["version"], expected_version);
    assert_eq!(identity["data"]["git_commit"], expected_commit);
    assert_eq!(identity["data"]["git_dirty"], expected_dirty);
    assert_ne!(identity["data"]["version"], "0.1.0");

    let output = Command::new(homeboy_bin())
        .arg("--version")
        .output()
        .expect("run shipped root binary version fast path");

    assert_eq!(output.status.code(), Some(0));
    let version = String::from_utf8(output.stdout).expect("version output is UTF-8");
    assert!(version.contains(expected_version));
    assert!(version.contains(&expected_commit));
    assert!(!version.contains("0.1.0"));
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git").args(args).output().expect("run git");
    assert!(output.status.success(), "git command failed");
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}
