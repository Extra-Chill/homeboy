use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const PATCH: &str =
    "diff --git a/src.txt b/src.txt\n--- a/src.txt\n+++ b/src.txt\n@@ -1 +1 @@\n-old\n+new\n";

#[test]
fn promotion_provider_receives_request_for_dry_run_apply_and_recoverable_gate_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace with spaces");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Test User"]);
    std::fs::write(workspace.join("src.txt"), "old\n").expect("source");
    git(&workspace, &["add", "src.txt"]);
    git(&workspace, &["commit", "-m", "initial"]);

    let patch = temp.path().join("changes.patch");
    std::fs::write(&patch, PATCH).expect("patch");
    let source = temp.path().join("outcome.json");
    std::fs::write(
        &source,
        serde_json::json!({
            "schema": "homeboy/agent-task-outcome/v1",
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": [{
                "schema": "homeboy/agent-task-artifact/v1",
                "id": "patch",
                "kind": "patch",
                "path": patch,
                "size_bytes": PATCH.len(),
                "sha256": sha256(PATCH),
            }],
        })
        .to_string(),
    )
    .expect("outcome");

    let dry_run = promote(&source, &workspace, true, None);
    assert_eq!(
        dry_run.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run = output(&dry_run);
    assert_eq!(dry_run["status"], "dry_run");
    assert_eq!(
        dry_run["command_evidence"][0]["command"][0],
        homeboy_bin().display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "old\n"
    );

    let applied = promote(&source, &workspace, false, None);
    assert_eq!(applied.status.code(), Some(0));
    let applied = output(&applied);
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["target"]["path"], workspace.display().to_string());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "new\n"
    );

    git(
        &workspace,
        &["apply", "-R", patch.to_str().expect("patch path")],
    );
    let failed = promote(&source, &workspace, false, Some("false"));
    assert_eq!(failed.status.code(), Some(1));
    let failed = output(&failed);
    assert_eq!(failed["status"], "gate_failed");
    assert_eq!(failed["target"]["path"], workspace.display().to_string());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "new\n"
    );
}

fn promote(
    source: &Path,
    workspace: &Path,
    dry_run: bool,
    verify: Option<&str>,
) -> std::process::Output {
    let binary = homeboy_bin();
    let mut command = Command::new(&binary);
    command
        .args(["--force-hot", "--allow-local-hot", "agent-task", "promote"])
        .arg(source)
        .args(["--to-worktree", "fixture@promotion-provider"])
        .arg(format!("--provider-argv={}", binary.display()))
        .arg("--provider-argv=agent-task")
        .arg("--provider-argv=promotion-provider")
        .arg(format!(
            "--provider-argv=--workspace={}",
            workspace.display()
        ))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    if dry_run {
        command.arg("--dry-run");
    }
    if let Some(verify) = verify {
        command.args(["--verify", verify]);
    }
    command.output().expect("run promotion")
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

fn sha256(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
