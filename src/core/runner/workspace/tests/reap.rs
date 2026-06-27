use std::fs;
use std::path::Path;

use crate::core::runner::workspace::sync::{reap_run_workspace, sync_workspace};
use crate::core::runner::workspace::types::{
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::core::runner::{MaterializedWorkspace, WorkspaceCleanupPolicy};

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
    crate::core::runner::create(
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
    crate::test_support::with_isolated_home(|_| {
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
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-reap-guard", runner_root.path());

        // A path under workspace_root but NOT under `_lab_workspaces` must be
        // refused by the containment guard, mirroring `prune_workspaces`.
        let outside = runner_root.path().join("not-a-lab-workspace");
        fs::create_dir_all(&outside).expect("outside dir");

        let result = reap_run_workspace(
            "lab-local-reap-guard",
            &outside.display().to_string(),
            None,
        );

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
    crate::test_support::with_isolated_home(|_| {
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
    crate::test_support::with_isolated_home(|_| {
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
    crate::test_support::with_isolated_home(|_| {
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
fn materialized_workspace_preserve_always_policy_never_reaps() {
    crate::test_support::with_isolated_home(|_| {
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
