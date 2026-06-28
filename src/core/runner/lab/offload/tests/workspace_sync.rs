use super::*;

fn synced_workspace(local_path: String) -> RunnerWorkspaceSyncOutput {
    RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: local_path.clone(),
        remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
        current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
            local_path: local_path.clone(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
        },
        workspace_lease: crate::core::runner::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path,
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot:workspace".to_string(),
        counts: crate::core::runner::ByteFileCounts::default(),
        excludes: Vec::new(),
        includes: Vec::new(),
        workspace_cleanliness: "snapshot_unique_workspace".to_string(),
        validation_dependencies: Vec::new(),
    }
}

fn source_snapshot(local_path: String) -> SourceSnapshot {
    SourceSnapshot {
        runner_id: "lab".to_string(),
        local_path: Some(local_path),
        remote_path: Some("/srv/homeboy/_lab_workspaces/app-abc".to_string()),
        workspace_root: None,
        git_branch: Some("main".to_string()),
        git_sha: Some("abc123".to_string()),
        dirty: false,
        sync_mode: "lab_offload".to_string(),
        workspace_snapshot_identity: Some("snapshot:workspace".to_string()),
        snapshot_hash: "sha256:source".to_string(),
        synced_at: "2026-06-28T00:00:00Z".to_string(),
        sync_excludes: Vec::new(),
    }
}

#[test]
fn lab_source_snapshot_handoff_accepts_matching_materialized_workspace() {
    let dir = tempfile::tempdir().expect("temp dir");
    let local_path = dir.path().canonicalize().unwrap().display().to_string();
    let synced = synced_workspace(local_path.clone());
    let snapshot = source_snapshot(local_path);

    validate_lab_source_snapshot_handoff(dir.path(), &synced, &snapshot)
        .expect("matching snapshot should pass");
}

#[test]
fn lab_source_snapshot_handoff_rejects_mismatched_source_paths() {
    let dir = tempfile::tempdir().expect("temp dir");
    let local_path = dir.path().canonicalize().unwrap().display().to_string();
    let synced = synced_workspace(local_path);
    let mut snapshot = source_snapshot("/Users/user/Developer/other-worktree".to_string());
    snapshot.remote_path = Some("/srv/homeboy/_lab_workspaces/other".to_string());
    snapshot.workspace_snapshot_identity = Some("snapshot:other".to_string());

    let err = validate_lab_source_snapshot_handoff(dir.path(), &synced, &snapshot)
        .expect_err("mismatched snapshot should fail before dispatch");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("source snapshot does not match"));
    let detail = err.details.to_string();
    assert!(detail.contains("requested_source_path"));
    assert!(detail.contains("snapshot_remote_path"));
    assert!(detail.contains("snapshot_workspace_identity"));
    assert!(err.details["tried"]
        .as_array()
        .expect("mismatch diagnostics")
        .iter()
        .any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("source snapshot remote_path"))));
}

#[test]
fn lab_git_workspace_sync_uses_snapshot_for_private_proxied_sources() {
    let source_policy = crate::core::runner::source_materialization::SourceMaterializationPolicy {
        private_proxied_source_hosts: vec!["github.example.com".to_string()],
    };
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.example.com:example-org/example-repo.git",
        ])
        .current_dir(dir.path())
        .status()
        .expect("add origin");

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    let mode = lab_workspace_sync_mode_with_source_policy(
        LabOffloadWorkspaceModePolicy::Git,
        &args,
        dir.path(),
        &source_policy,
    )
    .expect("sync mode");

    assert_eq!(mode, RunnerWorkspaceSyncMode::Snapshot);
}

#[test]
fn required_git_checkout_sync_keeps_git_for_private_sources() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.example.com:example-org/conductor.git",
        ])
        .current_dir(dir.path())
        .status()
        .expect("add origin");

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "prove it".to_string(),
    ];
    let mode = lab_workspace_sync_mode(
        LabOffloadWorkspaceModePolicy::GitCheckoutRequired,
        &args,
        dir.path(),
    )
    .expect("sync mode");

    assert_eq!(mode, RunnerWorkspaceSyncMode::Git);
}

#[test]
fn required_git_checkout_preflight_rejects_non_git_source_before_offload() {
    let dir = tempfile::tempdir().expect("temp dir");

    let err =
        preflight_patch_provider_git_checkout(dir.path()).expect_err("non-git source should fail");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("requires --cwd to be a git checkout"));
    assert!(err.details["tried"]
        .as_array()
        .expect("tried hints")
        .iter()
        .any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Homeboy worktree"))));
}

#[test]
fn required_git_checkout_preflight_rejects_checkout_without_origin() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");

    let err = preflight_patch_provider_git_checkout(dir.path())
        .expect_err("checkout without origin should fail");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("remote.origin.url"));
    assert!(err.details["tried"]
        .as_array()
        .expect("tried hints")
        .iter()
        .any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Set remote.origin.url"))));
}

#[test]
fn required_git_checkout_preflight_rejects_dirty_checkout() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/Extra-Chill/homeboy.git",
        ])
        .current_dir(dir.path())
        .status()
        .expect("add origin");
    std::fs::write(dir.path().join("dirty.txt"), "dirty").expect("write dirty file");

    let err =
        preflight_patch_provider_git_checkout(dir.path()).expect_err("dirty checkout should fail");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("clean git checkout"));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("clean task worktree"))));
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint.contains("dirty.txt"))));
    assert!(!tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("Commit or stash"))));
    assert!(!tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("--force-hot"))));
}

#[test]
fn required_git_checkout_preflight_accepts_clean_checkout_with_origin() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/Extra-Chill/homeboy.git",
        ])
        .current_dir(dir.path())
        .status()
        .expect("add origin");

    preflight_patch_provider_git_checkout(dir.path()).expect("clean checkout should pass");
}

#[test]
fn lab_git_workspace_sync_keeps_git_for_public_sources() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("init git repo");
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/Extra-Chill/homeboy.git",
        ])
        .current_dir(dir.path())
        .status()
        .expect("add origin");

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    let mode = lab_workspace_sync_mode(LabOffloadWorkspaceModePolicy::Git, &args, dir.path())
        .expect("sync mode");

    assert_eq!(mode, RunnerWorkspaceSyncMode::Git);
}

#[test]
fn in_flight_daemon_disconnect_error_surfaces_inspection_commands() {
    let source = Error::new(
        ErrorCode::InternalUnexpected,
        "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
        serde_json::json!({
            "runner_id": "homeboy-lab",
            "job_id": "job-123",
        }),
    );

    let err = in_flight_daemon_disconnect_error(
        "homeboy-lab",
        "job-123",
        None,
        "runner daemon health check failed",
        &source,
    );

    assert_eq!(err.code, ErrorCode::RunnerControllerDisconnected);
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.details["runner_id"], "homeboy-lab");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(err.details["status"], "recoverable_followup_required");
    assert_eq!(err.details["recovery"]["mode"], "durable_runner_job");
    assert_eq!(
        err.details["recovery"]["job_logs"],
        "homeboy runner job logs homeboy-lab job-123 --follow"
    );
    assert_eq!(
        err.details["recovery"]["runner_runs_list"],
        "homeboy runner exec homeboy-lab -- homeboy runs list --status running --limit 20"
    );
    assert_eq!(
        err.details["recovery"]["runner_run_artifacts"],
        "homeboy runner exec homeboy-lab -- homeboy runs artifacts <run-id>"
    );
    assert!(err.message.contains("still in flight"));
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains("homeboy runner exec homeboy-lab -- homeboy runs list --status running")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("homeboy runner exec homeboy-lab")));
}

#[test]
fn in_flight_daemon_disconnect_outcome_marks_durable_run_detached() {
    let source = Error::new(
        ErrorCode::InternalUnexpected,
        "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
        serde_json::json!({
            "runner_id": "homeboy-lab",
            "job_id": "job-123",
        }),
    );

    let outcome = in_flight_daemon_disconnect_outcome(
        base_lab_plan(Some(&portable_lab_command("agent-task cook"))),
        "homeboy-lab",
        "job-123",
        "run-123",
        "runner daemon health check failed",
        &source,
    );

    let LabOffloadOutcome::Offloaded {
        plan,
        stdout,
        stderr,
        exit_code,
        output_file_content,
    } = outcome
    else {
        panic!("expected detached offloaded outcome");
    };

    assert_eq!(exit_code, 0);
    assert!(output_file_content.is_none());
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
    assert_eq!(json["success"], serde_json::json!(true));
    assert_eq!(json["data"]["status"], "dispatched_detached");
    assert_eq!(json["data"]["followup_required"], true);
    assert_eq!(json["data"]["durable_run_id"], "run-123");
    assert_eq!(json["data"]["runner_id"], "homeboy-lab");
    assert_eq!(json["data"]["job_id"], "job-123");
    assert_eq!(
        json["data"]["retrieval_commands"]["status"],
        "homeboy agent-task status run-123"
    );
    assert!(stderr.contains("durable agent-task run `run-123` continues remotely"));
    assert!(stderr.contains("homeboy agent-task logs run-123"));
    assert!(plan.steps.iter().any(
        |step| step.id == "lab.exec.detached" && step.status == PlanStepStatus::PartialSuccess
    ));
}
