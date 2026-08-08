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
    let expected_binary = homeboy_bin().to_string_lossy().into_owned();
    assert_eq!(
        identity["data"]["active_binary"].as_str(),
        Some(expected_binary.as_str())
    );
    assert_ne!(identity["data"]["version"], "0.1.0");

    let inspect = Command::new(homeboy_bin())
        .args(["self", "inspect"])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run identity discovery alias");

    assert_eq!(inspect.status.code(), Some(0));
    assert_eq!(inspect.stdout, output.stdout);

    let self_help = Command::new(homeboy_bin())
        .args(["self", "--help"])
        .output()
        .expect("run self help");
    assert_eq!(self_help.status.code(), Some(0));
    let self_help = String::from_utf8(self_help.stdout).expect("self help is UTF-8");
    assert!(self_help.contains("identity"));
    assert!(self_help.contains("without external probes"));

    let root_help = Command::new(homeboy_bin())
        .arg("--help")
        .output()
        .expect("run root help");
    assert_eq!(root_help.status.code(), Some(0));
    let root_help = String::from_utf8(root_help.stdout).expect("root help is UTF-8");
    assert!(root_help.contains("self identity"));

    let typo = Command::new(homeboy_bin())
        .args(["self", "identit"])
        .output()
        .expect("run identity typo");
    assert_eq!(typo.status.code(), Some(2));
    let typo = String::from_utf8(typo.stderr).expect("typo guidance is UTF-8");
    assert!(
        typo.contains("identity"),
        "missing identity guidance: {typo}"
    );

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
