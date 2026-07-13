use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PATCH: &str =
    "diff --git a/src.txt b/src.txt\n--- a/src.txt\n+++ b/src.txt\n@@ -1 +1 @@\n-old\n+new\n";

#[test]
fn promotion_provider_validates_git_workspace_and_applies_patch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace with spaces");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Test User"]);
    std::fs::write(workspace.join("src.txt"), "old\n").expect("source");
    git(&workspace, &["add", "src.txt"]);
    git(&workspace, &["commit", "-m", "initial"]);
    assert_eq!(
        git_output(&workspace, &["rev-parse", "--is-inside-work-tree"]),
        "true"
    );

    let patch = temp.path().join("changes.patch");
    std::fs::write(&patch, PATCH).expect("patch");
    let dry_run = promotion_provider(&workspace, &patch, true);
    assert_eq!(
        dry_run.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run = output(&dry_run);
    assert_eq!(
        dry_run["schema"],
        "homeboy/agent-task-promotion-apply-response/v1"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "old\n"
    );

    let applied = promotion_provider(&workspace, &patch, false);
    assert_eq!(applied.status.code(), Some(0));
    let applied = output(&applied);
    assert_eq!(applied["workspace_path"], workspace.display().to_string());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "new\n"
    );
}

fn promotion_provider(workspace: &Path, patch: &Path, dry_run: bool) -> std::process::Output {
    let binary = homeboy_bin();
    let mut process = Command::new(binary)
        .args(["agent-task", "promotion-provider", "--workspace"])
        .arg(workspace)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start promotion provider");
    process
        .stdin
        .as_mut()
        .expect("provider stdin")
        .write_all(
            serde_json::json!({
                "schema": "homeboy/agent-task-promotion-apply-request/v1",
                "to_workspace": "fixture@promotion-provider",
                "patch_path": patch,
                "changed_files": ["src.txt"],
                "dry_run": dry_run,
            })
            .to_string()
            .as_bytes(),
        )
        .expect("write provider request");
    process.wait_with_output().expect("run promotion provider")
}

fn output(output: &std::process::Output) -> Value {
    let value: Value = serde_json::from_slice(&output.stdout).expect("promotion JSON");
    assert_eq!(value["success"], output.status.success());
    value["data"].clone()
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
