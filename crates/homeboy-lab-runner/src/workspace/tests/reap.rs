use std::fs;
use std::path::Path;

use crate::workspace::sync::{reap_run_workspace, sync_workspace, WORKSPACE_METADATA_FILE};
use crate::workspace::types::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use crate::{MaterializedWorkspace, WorkspaceCleanupPolicy, WorkspaceTerminalOutcome};

fn sync_options(path: String) -> RunnerWorkspaceSyncOptions {
    RunnerWorkspaceSyncOptions {
        path,
        mode: RunnerWorkspaceSyncMode::Snapshot,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        run_isolation_token: None,
    }
}

fn create_local_runner(id: &str, root: &Path) {
    crate::create(
        &format!(
            r#"{{"id":"{id}","kind":"local","workspace_root":"{}"}}"#,
            root.display()
        ),
        false,
    )
    .expect("create runner");
}

/// Sync a fresh local runner workspace and return its remote checkout path.
fn sync_local_workspace(runner_id: &str, runner_root: &Path) -> String {
    let source_parent = tempfile::tempdir().expect("source parent");
    let source = source_parent.path().join("reap-source");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("file.txt"), "hello\n").expect("source file");
    create_local_runner(runner_id, runner_root);
    let (synced, _) = sync_workspace(runner_id, sync_options(source.display().to_string()))
        .expect("sync workspace");
    synced.remote_path
}

#[test]
fn reap_run_workspace_removes_checkout_and_artifact_sibling() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-reap", runner_root.path());

        // The Homeboy-owned structured-output artifact dir is a sibling of the
        // checkout (`<checkout>-homeboy-artifacts`), created only when the run
        // requested `--output`. Reap must remove it alongside the checkout.
        let artifact_dir = format!("{remote_path}-homeboy-artifacts");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(Path::new(&artifact_dir).join("out.json"), "{}").expect("artifact file");
        assert!(Path::new(&remote_path).exists());
        assert!(Path::new(&artifact_dir).exists());

        reap_run_workspace("lab-local-reap", &remote_path, Some(&artifact_dir)).expect("reap");

        assert!(!Path::new(&remote_path).exists(), "checkout was not reaped");
        assert!(
            !Path::new(&artifact_dir).exists(),
            "artifact sibling was not reaped"
        );
    });
}

#[test]
fn reap_run_workspace_refuses_paths_outside_lab_workspaces() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-reap-guard", runner_root.path());

        // A path under workspace_root but NOT under `_lab_workspaces` must be
        // refused by the containment guard, mirroring `prune_workspaces`.
        let outside = runner_root.path().join("not-a-lab-workspace");
        fs::create_dir_all(&outside).expect("outside dir");

        let result =
            reap_run_workspace("lab-local-reap-guard", &outside.display().to_string(), None);

        assert!(
            result.is_err(),
            "reap must refuse a path outside _lab_workspaces"
        );
        assert!(
            outside.exists(),
            "the containment guard must not delete an out-of-root path"
        );
    });
}

#[test]
fn materialized_workspace_reaps_on_success_under_default_policy() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-success", runner_root.path());
        assert!(Path::new(&remote_path).exists());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-success".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::default(),
            );
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Success);
        } // drop reaps under the default delete-on-success policy

        assert!(
            !Path::new(&remote_path).exists(),
            "success path must reap the run-scoped workspace"
        );
    });
}

#[test]
fn materialized_workspace_preserves_on_failure_under_explicit_debug_policy() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-failure", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-failure".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveOnFailure,
            );
            // A failed run is the default outcome (success never recorded).
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Failure);
        } // drop preserves the workspace for post-mortem evidence

        assert!(
            Path::new(&remote_path).exists(),
            "failure path must preserve the run-scoped workspace as evidence"
        );
    });
}

#[test]
fn terminal_evidence_records_retained_outcome_owner_location_and_reclaim_command() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        for (outcome, expected_owner) in [
            (WorkspaceTerminalOutcome::Failure, "runner.workspace"),
            (WorkspaceTerminalOutcome::Cancelled, "runner.workspace"),
            (WorkspaceTerminalOutcome::UncertainHandoff, "runner.job"),
        ] {
            let runner_id = format!("lab-local-terminal-evidence-{}", outcome.label());
            let remote_path = sync_local_workspace(&runner_id, runner_root.path());
            {
                let mut handle = MaterializedWorkspace::new(
                    runner_id.clone(),
                    remote_path.clone(),
                    None,
                    WorkspaceCleanupPolicy::PreserveOnFailure,
                );
                if outcome == WorkspaceTerminalOutcome::UncertainHandoff {
                    handle.preserve();
                } else {
                    handle.set_terminal_outcome(outcome);
                }
            }

            let metadata =
                fs::read_to_string(Path::new(&remote_path).join(WORKSPACE_METADATA_FILE))
                    .expect("terminal metadata");
            let metadata: serde_json::Value =
                serde_json::from_str(&metadata).expect("metadata json");
            let evidence = &metadata["terminal_evidence"];
            assert_eq!(evidence["policy"], "preserve-on-failure");
            assert_eq!(evidence["final_outcome"], outcome.label());
            assert_eq!(evidence["lifecycle_owner"], expected_owner);
            assert_eq!(evidence["retained_location"], remote_path);
            assert_eq!(metadata["resource_lifecycle"]["status"], "retained");
            if outcome == WorkspaceTerminalOutcome::UncertainHandoff {
                assert_eq!(metadata["resource_lifecycle"]["cleanup_policy"], "preserve");
                assert!(metadata["resource_lifecycle"]["ttl"].is_null());
                assert!(evidence["reclaim_command"].is_null());
            } else {
                assert_eq!(
                    evidence["reclaim_command"],
                    format!("homeboy runner workspace prune {runner_id} --apply --min-age-hours 0")
                );
            }
        }
    });
}

#[test]
fn preserve_on_failure_keeps_cancelled_workspace_for_ttl_reclamation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-cancelled", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-cancelled".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveOnFailure,
            );
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Cancelled);
        }

        assert!(
            Path::new(&remote_path).exists(),
            "cancellation must retain the workspace under preserve-on-failure"
        );
        let metadata = fs::read_to_string(Path::new(&remote_path).join(WORKSPACE_METADATA_FILE))
            .expect("cancelled terminal metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["terminal_evidence"]["final_outcome"], "cancelled");
    });
}

#[test]
fn preserve_on_failure_keeps_workspace_when_unwinding_from_panic() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-panic", runner_root.path());

        let result = std::panic::catch_unwind(|| {
            let _handle = MaterializedWorkspace::new(
                "lab-local-mat-panic".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveOnFailure,
            );
            panic!("test unwind");
        });

        assert!(result.is_err());
        assert!(Path::new(&remote_path).exists());
        let metadata = fs::read_to_string(Path::new(&remote_path).join(WORKSPACE_METADATA_FILE))
            .expect("panic terminal metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["terminal_evidence"]["final_outcome"], "panic");
        assert_eq!(
            metadata["terminal_evidence"]["retained_location"],
            remote_path
        );
    });
}

#[test]
fn delete_on_terminal_keeps_workspace_when_unwinding_from_panic() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-default-panic", runner_root.path());

        let result = std::panic::catch_unwind(|| {
            let _handle = MaterializedWorkspace::new(
                "lab-local-mat-default-panic".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::DeleteAlways,
            );
            panic!("test unwind");
        });

        assert!(result.is_err());
        assert!(Path::new(&remote_path).exists());
        let metadata = fs::read_to_string(Path::new(&remote_path).join(WORKSPACE_METADATA_FILE))
            .expect("panic terminal metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["terminal_evidence"]["final_outcome"], "panic");
        assert_eq!(metadata["resource_lifecycle"]["status"], "retained");
    });
}

#[test]
fn materialized_workspace_preserve_disarms_reap_even_on_success() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-detach", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-detach".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::default(),
            );
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Success);
            // A detached/in-flight remote job still owns the workspace.
            handle.preserve();
        }

        assert!(
            Path::new(&remote_path).exists(),
            "preserve() must hand off ownership without reaping, even on success"
        );
    });
}

#[test]
fn delete_always_workspace_preserved_on_retryable_admission_failure_is_not_reaped() {
    // #9469: the Lab offload workspace uses DeleteAlways so genuine terminal
    // outcomes always release the admitted rig/extension snapshots. But a
    // retryable pre-acceptance admission failure calls preserve() on it so the
    // already-staged rig install/sync + snapshots survive for a retry to resume
    // — even under DeleteAlways. Proves preserve() disarms the reap the offload
    // path would otherwise perform.
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-retry", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-retry".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::DeleteAlways,
            );
            // A retryable admission failure preserves the prepared workspace.
            handle.preserve();
        } // drop must NOT reap despite DeleteAlways

        assert!(
            Path::new(&remote_path).exists(),
            "a retryable admission failure must preserve the prepared workspace for resume, \
             even under DeleteAlways"
        );
    });
}

#[test]
fn materialized_workspace_preserve_always_policy_never_reaps() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-preserve", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-preserve".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveAlways,
            );
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Success);
        }

        assert!(
            Path::new(&remote_path).exists(),
            "PreserveAlways must never auto-reap, even on success"
        );
    });
}

#[test]
fn job_runtime_cleanup_reaps_success_failure_and_cancellation_without_touching_runner_defaults() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let defaults = runner_root
            .path()
            .join(".config/homeboy/extensions/default");
        fs::create_dir_all(defaults.parent().expect("default parent")).expect("default parent");
        fs::write(&defaults, "runner default").expect("runner default");

        for outcome in ["success", "failure", "cancelled"] {
            let runner_id = format!("lab-local-terminal-{outcome}");
            let remote_path = sync_local_workspace(&runner_id, runner_root.path());
            let artifact_dir = format!("{remote_path}-homeboy-artifacts");
            fs::create_dir_all(format!("{artifact_dir}/rig-registry/same-id")).expect("rig state");
            fs::create_dir_all(format!(
                "{artifact_dir}/extension-runtime/home/.config/homeboy/extensions"
            ))
            .expect("extension state");

            {
                let mut handle = MaterializedWorkspace::new(
                    runner_id,
                    remote_path.clone(),
                    Some(artifact_dir.clone()),
                    WorkspaceCleanupPolicy::DeleteAlways,
                );
                handle.set_terminal_outcome(if outcome == "success" {
                    WorkspaceTerminalOutcome::Success
                } else if outcome == "cancelled" {
                    WorkspaceTerminalOutcome::Cancelled
                } else {
                    WorkspaceTerminalOutcome::Failure
                });
            }

            assert!(
                !Path::new(&remote_path).exists(),
                "{outcome} checkout was not reaped"
            );
            assert!(
                !Path::new(&artifact_dir).exists(),
                "{outcome} job runtime was not reaped"
            );
            assert_eq!(
                fs::read_to_string(&defaults).expect("runner default survives"),
                "runner default"
            );
        }
    });
}

#[test]
fn concurrent_same_ref_jobs_keep_the_remaining_job_workspace_after_peer_reap() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("same-ref-source");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "same source revision\n").expect("source file");
        create_local_runner("lab-local-concurrent-ownership", runner_root.path());

        let mut first_options = sync_options(source.display().to_string());
        first_options.run_isolation_token = Some("job-lint".to_string());
        let mut second_options = sync_options(source.display().to_string());
        second_options.run_isolation_token = Some("job-test".to_string());
        let (first, _) = sync_workspace("lab-local-concurrent-ownership", first_options)
            .expect("materialize lint workspace");
        let (second, _) = sync_workspace("lab-local-concurrent-ownership", second_options)
            .expect("materialize test workspace");

        assert_ne!(first.remote_path, second.remote_path);
        assert!(Path::new(&first.remote_path).join("file.txt").is_file());
        assert!(Path::new(&second.remote_path).join("file.txt").is_file());
        let metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(Path::new(&second.remote_path).join(WORKSPACE_METADATA_FILE))
                .expect("second metadata"),
        )
        .expect("metadata json");
        assert_eq!(metadata["run_id"], "job-test");
        assert!(metadata["workspace_lease"].as_str().is_some());

        {
            let mut first_handle = MaterializedWorkspace::new(
                "lab-local-concurrent-ownership".to_string(),
                first.remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::DeleteAlways,
            );
            first_handle.set_terminal_outcome(WorkspaceTerminalOutcome::Success);
        }

        assert!(!Path::new(&first.remote_path).exists());
        assert!(Path::new(&second.remote_path).is_dir());
        assert_eq!(
            fs::read_to_string(Path::new(&second.remote_path).join("file.txt"))
                .expect("second job cwd remains readable"),
            "same source revision\n"
        );

        {
            let mut second_handle = MaterializedWorkspace::new(
                "lab-local-concurrent-ownership".to_string(),
                second.remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::DeleteAlways,
            );
            second_handle.set_terminal_outcome(WorkspaceTerminalOutcome::Success);
        }
        assert!(!Path::new(&second.remote_path).exists());
    });
}
