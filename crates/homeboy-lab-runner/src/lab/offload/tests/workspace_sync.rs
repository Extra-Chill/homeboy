use super::*;

fn synced_workspace(local_path: String) -> RunnerWorkspaceSyncOutput {
    RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: local_path.clone(),
        remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
        materialization_plan: RunnerWorkspaceMaterializationPlan::from_test_parts(
            "/srv/homeboy",
            &local_path,
            "app",
            "/srv/homeboy/_lab_workspaces/app-abc",
            RunnerWorkspaceSyncMode::Snapshot,
            "snapshot:workspace",
        ),
        current_workspace: crate::RunnerWorkspaceCurrentSummary {
            local_path: local_path.clone(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
        },
        workspace_lease: crate::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path,
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        resource_lifecycle: crate::workspace_resource_lifecycle(
            "lab",
            "/srv/homeboy/_lab_workspaces/app-abc",
            None,
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
        ),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot:workspace".to_string(),
        content: None,
        workspace_ref: "workspace:00000000-0000-0000-0000-000000000001".to_string(),
        prepared_workspace_lease: None,
        counts: crate::ByteFileCounts::default(),
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
        prepared_workspace_original_snapshot_identity: None,
        prepared_workspace_update_lineage: Vec::new(),
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
        snapshot_hash: "sha256:source".to_string(),
        synced_at: "2026-06-28T00:00:00Z".to_string(),
        sync_excludes: Vec::new(),
        repository_integrity_evidence: None,
    }
}

#[test]
fn lab_source_snapshot_handoff_requires_the_materialized_workspace() {
    let dir = tempfile::tempdir().expect("temp dir");
    let local_path = dir.path().canonicalize().unwrap().display().to_string();
    let synced = synced_workspace(local_path.clone());

    validate_lab_source_snapshot_handoff(dir.path(), &synced, &source_snapshot(local_path))
        .expect("matching snapshot should pass");

    let mut snapshot = source_snapshot("/Users/user/Developer/other-worktree".to_string());
    snapshot.remote_path = Some("/srv/homeboy/_lab_workspaces/other".to_string());
    snapshot.workspace_snapshot_identity = Some("snapshot:other".to_string());
    let err = validate_lab_source_snapshot_handoff(dir.path(), &synced, &snapshot)
        .expect_err("mismatched snapshot should fail before dispatch");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("source snapshot does not match"));
    let detail = err.details.to_string();
    for field in [
        "requested_source_path",
        "snapshot_remote_path",
        "snapshot_workspace_identity",
    ] {
        assert!(detail.contains(field), "missing {field}: {detail}");
    }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A fresh git checkout with an optional `origin` remote.
fn git_checkout(origin: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    if let Some(origin) = origin {
        git(dir.path(), &["remote", "add", "origin", origin]);
    }
    dir
}

fn tried_hints(err: &Error) -> Vec<String> {
    err.details["tried"]
        .as_array()
        .expect("tried hints")
        .iter()
        .filter_map(|hint| hint.as_str().map(str::to_string))
        .collect()
}

const PUBLIC_ORIGIN: &str = "https://github.com/Extra-Chill/homeboy.git";
const PRIVATE_ORIGIN: &str = "git@github.example.com:example-org/example-repo.git";

#[test]
fn lab_workspace_sync_mode_follows_source_privacy_and_policy() {
    let private_policy = crate::source_materialization::SourceMaterializationPolicy {
        private_proxied_source_hosts: vec!["github.example.com".to_string()],
    };
    let args = ["homeboy", "agent-task", "cook"].map(str::to_string);
    for (origin, policy, expected) in [
        // Private proxied sources cannot be cloned by the runner.
        (
            PRIVATE_ORIGIN,
            LabOffloadWorkspaceModePolicy::Git,
            RunnerWorkspaceSyncMode::Snapshot,
        ),
        (
            PUBLIC_ORIGIN,
            LabOffloadWorkspaceModePolicy::Git,
            RunnerWorkspaceSyncMode::Git,
        ),
        // A required git checkout keeps git even for private sources.
        (
            PRIVATE_ORIGIN,
            LabOffloadWorkspaceModePolicy::GitCheckoutRequired,
            RunnerWorkspaceSyncMode::Git,
        ),
    ] {
        let dir = git_checkout(Some(origin));
        let mode =
            lab_workspace_sync_mode_with_source_policy(policy, &args, dir.path(), &private_policy)
                .expect("sync mode");
        assert_eq!(mode, expected, "{origin} {policy:?}");
    }
}

#[test]
fn lab_source_snapshot_uses_git_workspace_for_committed_git_source() {
    let dir = git_checkout(None);
    std::fs::write(dir.path().join("package.json"), "{}\n").expect("source file");
    git(dir.path(), &["add", "."]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=Homeboy Test",
            "-c",
            "user.email=test@homeboy.invalid",
            "commit",
            "--quiet",
            "-m",
            "source",
        ],
    );

    let mode = lab_workspace_sync_mode(
        LabOffloadWorkspaceModePolicy::RunnerResident,
        &[],
        dir.path(),
    )
    .expect("sync mode");

    assert_eq!(mode, RunnerWorkspaceSyncMode::SnapshotGit);
}

#[test]
fn required_git_checkout_preflight_requires_a_clean_checkout_with_origin() {
    let clean = git_checkout(Some(PUBLIC_ORIGIN));
    preflight_patch_provider_git_checkout(clean.path()).expect("clean checkout should pass");

    let non_git = tempfile::tempdir().expect("temp dir");
    let without_origin = git_checkout(None);
    let dirty = git_checkout(Some(PUBLIC_ORIGIN));
    std::fs::write(dirty.path().join("dirty.txt"), "dirty").expect("write dirty file");

    for (dir, message, hints) in [
        (
            non_git.path(),
            "requires --cwd to be a git checkout",
            &["Homeboy worktree"][..],
        ),
        (
            without_origin.path(),
            "remote.origin.url",
            &["Set remote.origin.url"][..],
        ),
        (
            dirty.path(),
            "clean git checkout",
            &["clean task worktree", "dirty.txt"][..],
        ),
    ] {
        let err = preflight_patch_provider_git_checkout(dir).expect_err("preflight must refuse");
        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains(message), "{}", err.message);
        let tried = tried_hints(&err);
        for hint in hints {
            assert!(tried.iter().any(|tried| tried.contains(hint)), "{tried:?}");
        }
        // A dirty task worktree is never solved by stashing or running locally.
        assert!(!tried
            .iter()
            .any(|hint| hint.contains("Commit or stash") || hint.contains("--placement local")));
    }
}

/// Regression for #16166: changed-since staging must name the directory its
/// remote read checked. A checkout without origin is caller input with a fix
/// hint; a non-git directory (daemon cwd, scratch dir, deleted checkout) is a
/// controller staging failure classified as infrastructure (internal.io).
#[test]
fn changed_since_git_sync_mode_names_the_checked_directory() {
    let args = [
        "homeboy",
        "review",
        "test",
        "--changed-since",
        "origin/main",
    ]
    .map(str::to_string);
    let without_origin = git_checkout(None);
    let non_git = tempfile::tempdir().expect("temp dir");

    for (dir, code, message) in [
        (
            without_origin.path(),
            ErrorCode::ValidationInvalidArgument,
            "remote.origin.url",
        ),
        (
            non_git.path(),
            ErrorCode::InternalIoError,
            "not a Git work tree",
        ),
    ] {
        let err = lab_workspace_sync_mode(
            LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            &args,
            dir,
        )
        .expect_err("changed-since staging must fail with named evidence");

        assert_eq!(err.code, code);
        assert!(
            err.message.contains(&dir.display().to_string()),
            "error must name the directory it checked: {}",
            err.message
        );
        assert!(err.message.contains(message), "{}", err.message);
        if code == ErrorCode::ValidationInvalidArgument {
            assert!(tried_hints(&err)
                .iter()
                .any(|hint| hint.contains("Set remote.origin.url")));
        }
    }
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
}

#[test]
fn in_flight_daemon_disconnect_outcome_marks_durable_run_detached() {
    crate::test_support::with_isolated_home(|_| {
        let source = Error::new(
            ErrorCode::InternalUnexpected,
            "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
            serde_json::json!({
                "runner_id": "homeboy-lab",
                "job_id": "job-123",
            }),
        );
        let remote_command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let outcome = in_flight_daemon_disconnect_outcome(
            base_lab_plan(Some(&portable_lab_command("agent-task cook"))),
            "homeboy-lab",
            "job-123",
            "run-123",
            "/runner/workspace/homeboy",
            &remote_command,
            "runner daemon health check failed",
            &source,
        )
        .expect("detached lifecycle record");

        let LabOffloadOutcome::InFlight {
            plan,
            stdout,
            stderr,
            exit_code,
            output_file_content,
        } = outcome
        else {
            panic!("expected in-flight handoff outcome");
        };

        assert_eq!(exit_code, 0);
        assert!(output_file_content.is_some());
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
        assert_eq!(json["success"], serde_json::json!(true));
        assert_eq!(json["data"]["status"], "remote_in_flight");
        assert_eq!(json["data"]["followup_required"], true);
        assert_eq!(json["data"]["durable_run_id"], "run-123");
        assert_eq!(json["data"]["runner_id"], "homeboy-lab");
        assert_eq!(json["data"]["job_id"], "job-123");
        assert_eq!(
            json["data"]["retrieval_commands"]["status"],
            "homeboy agent-task status run-123"
        );
        assert!(stderr.contains("durable agent-task run `run-123` continues remotely"));
        assert!(stderr.contains("homeboy runs logs run-123"));
        assert!(plan
            .steps
            .iter()
            .any(|step| step.id == "lab.exec.detached"
                && step.status == PlanStepStatus::PartialSuccess));
        let record = homeboy_agents::agent_task_lifecycle::reconcile_status("run-123")
            .expect("inspectable agent-task record");
        assert_eq!(record.metadata["runner_id"], "homeboy-lab");
        assert_eq!(record.metadata["runner_job_id"], "job-123");
        assert_eq!(record.metadata["phase"], "awaiting_runner_result");
        assert_eq!(
            record.metadata["runner_handoff"]["authority"],
            "runner_daemon"
        );
    });
}

#[test]
fn controller_wait_expiry_identifies_only_the_durable_handoff() {
    let timeout = Error::new(
        ErrorCode::InternalUnexpected,
        "runner daemon job job-123 did not finish before timeout",
        serde_json::json!({
            "reason": "controller_wait_expired",
            "job_id": "job-123",
        }),
    );
    assert_eq!(controller_wait_expired_job_id(&timeout), Some("job-123"));

    let remote_failure = Error::new(
        ErrorCode::InternalUnexpected,
        "remote command failed",
        serde_json::json!({ "job_id": "job-123" }),
    );
    assert_eq!(controller_wait_expired_job_id(&remote_failure), None);
}
