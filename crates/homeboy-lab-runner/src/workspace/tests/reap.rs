use std::fs;
use std::path::Path;

use crate::workspace::sync::{reap_run_workspace, sync_workspace};
use crate::workspace::types::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use crate::{MaterializedWorkspace, WorkspaceCleanupPolicy};

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
            handle.set_success(true);
        } // drop reaps under the default delete-on-success policy

        assert!(
            !Path::new(&remote_path).exists(),
            "success path must reap the run-scoped workspace"
        );
    });
}

#[test]
fn materialized_workspace_preserves_on_failure_under_default_policy() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let remote_path = sync_local_workspace("lab-local-mat-failure", runner_root.path());

        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-mat-failure".to_string(),
                remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::default(),
            );
            // A failed run is the default outcome (success never recorded).
            handle.set_success(false);
        } // drop preserves the workspace for post-mortem evidence

        assert!(
            Path::new(&remote_path).exists(),
            "failure path must preserve the run-scoped workspace as evidence"
        );
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
            handle.set_success(true);
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
            handle.set_success(true);
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
                handle.set_success(outcome == "success");
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
