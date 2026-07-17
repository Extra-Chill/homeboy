use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PATCH: &str =
    "diff --git a/src.txt b/src.txt\n--- a/src.txt\n+++ b/src.txt\n@@ -1 +1 @@\n-old\n+new\n";

#[test]
fn promotion_provider_applies_a_typed_patch_request() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("materialized managed target");
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
    assert_eq!(
        applied.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&applied.stdout),
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied = output(&applied);
    assert_eq!(applied["workspace_path"], workspace.display().to_string());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "new\n"
    );
}

#[cfg(unix)]
#[test]
fn promotion_gate_binds_a_socket_in_the_short_invocation_tmpdir_for_a_long_run_id() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Test User"]);
    std::fs::write(workspace.join("src.txt"), "old\n").expect("source");
    git(&workspace, &["add", "src.txt"]);
    git(&workspace, &["commit", "-m", "initial"]);
    let patch = temp.path().join("changes.patch");
    std::fs::write(&patch, PATCH).expect("patch");

    let provider = temp.path().join("promotion-provider.sh");
    std::fs::write(
        &provider,
        format!(
            "#!/bin/sh\nset -eu\ngit -C '{}' apply '{}'\nprintf '%s\\n' '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{}\"}}'\n",
            workspace.display(),
            patch.display(),
            workspace.display(),
        ),
    )
    .expect("provider script");
    let mut permissions = std::fs::metadata(&provider)
        .expect("provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&provider, permissions).expect("provider executable");

    let helper_source = temp.path().join("bind_socket.rs");
    let helper = temp.path().join("bind_socket");
    std::fs::write(
        &helper_source,
        "use std::{env, os::unix::net::UnixListener, path::PathBuf};\nfn main() { let path = PathBuf::from(env::var_os(\"TMPDIR\").unwrap()).join(\"gate.sock\"); UnixListener::bind(&path).unwrap(); println!(\"{}\", path.display()); }\n",
    )
    .expect("socket helper source");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    assert!(
        Command::new(rustc)
            .args(["--edition=2021"])
            .arg(&helper_source)
            .arg("-o")
            .arg(&helper)
            .status()
            .expect("compile socket helper")
            .success(),
        "socket helper compiles"
    );
    let run_id = format!("promotion-run-{}", "semantic-id-".repeat(16));
    let source = serde_json::json!({
        "schema": "homeboy/agent-task-outcome/v1",
        "task_id": "socket-gate",
        "status": "succeeded",
        "artifacts": [{ "schema": "homeboy/agent-task-artifact/v1", "id": "patch", "kind": "patch", "path": patch }]
    })
    .to_string();

    let report = homeboy::core::agent_task_promotion::promote(
        homeboy::core::agent_task_promotion::AgentTaskPromotionOptions {
            source,
            source_run_id: Some(run_id.clone()),
            source_path: None,
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@socket-gate".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: homeboy::core::agent_task_gate::VerifyGateOptions {
                verify: vec![helper.display().to_string()],
                private_verify: Vec::new(),
                private_gate_reveal:
                    homeboy::core::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
            },
            provider_command: Some(provider.display().to_string()),
            provider_invocation: None,
        },
    )
    .expect("promotion and socket gate succeed");

    assert_eq!(
        report.status,
        homeboy::core::agent_task_promotion::AgentTaskPromotionStatus::Applied
    );
    let socket_path = report.deterministic_gates[0].stdout.trim();
    assert!(socket_path.ends_with("gate.sock"));
    assert!(socket_path.len() < 104, "socket path: {socket_path}");
    assert!(!socket_path.contains(&run_id));
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
