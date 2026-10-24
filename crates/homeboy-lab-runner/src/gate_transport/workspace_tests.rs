//! Lifecycle of the Lab workspaces a gate materializes (resource package and
//! candidate snapshot). They are owned by the gate's controller run and must
//! not outlive its authoritatively observed terminal outcome.

use super::*;
use std::fs;

const RUNNER: &str = "lab-local-gate-workspace";
const RUN_ID: &str = "lab-gate-workspace-owner";

fn local_runner(root: &Path) {
    crate::create(
        &format!(
            r#"{{"id":"{RUNNER}","kind":"local","workspace_root":"{}"}}"#,
            root.display()
        ),
        false,
    )
    .expect("create runner");
}

fn resource_package(parent: &Path) -> std::path::PathBuf {
    let root = parent.join("declared-gate-resources");
    fs::create_dir_all(&root).expect("resource root");
    fs::write(root.join("resources.json"), b"{}").expect("resource descriptor");
    root
}

fn metadata(remote_path: &str) -> serde_json::Value {
    serde_json::from_str(
        &fs::read_to_string(
            homeboy_lab_contract::lab::handoff::runner_workspace_metadata_path(&remote_path),
        )
        .expect("workspace metadata"),
    )
    .expect("metadata json")
}

#[test]
fn completed_gate_releases_its_run_owned_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        let controller = tempfile::tempdir().expect("controller tmp");
        local_runner(runner_root.path());
        let resources = resource_package(controller.path());

        let mut workspace = materialize_gate_workspace(
            RUNNER,
            &resources,
            crate::RunnerWorkspaceSyncMode::Snapshot,
            RUN_ID,
        )
        .expect("materialize gate workspace");
        let remote_path = workspace.sync.remote_path.clone();
        let lease = metadata(&remote_path);
        assert_eq!(
            lease["run_id"], RUN_ID,
            "the gate's durable controller run owns its workspace lease"
        );
        assert_eq!(lease["resource_lifecycle"]["run_id"], RUN_ID);

        workspace.admitted("gate-job-1", Some("generation-1"));
        workspace.terminal(crate::WorkspaceTerminalOutcome::Success);
        drop(workspace);

        assert!(
            !Path::new(&remote_path).exists(),
            "a completed gate left its workspace behind with lease {}",
            fs::read_to_string(
                homeboy_lab_contract::lab::handoff::runner_workspace_metadata_path(&remote_path)
            )
            .unwrap_or_default()
        );
    });
}

#[test]
fn gate_without_terminal_outcome_retains_workspace_for_job_reconciliation() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        let controller = tempfile::tempdir().expect("controller tmp");
        local_runner(runner_root.path());
        let resources = resource_package(controller.path());

        let mut workspace = materialize_gate_workspace(
            RUNNER,
            &resources,
            crate::RunnerWorkspaceSyncMode::Snapshot,
            RUN_ID,
        )
        .expect("materialize gate workspace");
        let remote_path = workspace.sync.remote_path.clone();
        // The accepted job's terminal state was never observed (controller
        // lost the runner): the live job may still read this workspace.
        workspace.admitted("gate-job-2", Some("generation-2"));
        drop(workspace);

        assert!(Path::new(&remote_path).exists());
        let lease = metadata(&remote_path);
        assert_eq!(lease["job_id"], "gate-job-2");
        assert_eq!(lease["resource_lifecycle"]["status"], "retained");
        assert_eq!(
            lease["resource_lifecycle"]["cleanup_policy"],
            "delete_after_ttl"
        );
    });
}

#[test]
fn gate_failing_before_job_admission_releases_its_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        let controller = tempfile::tempdir().expect("controller tmp");
        local_runner(runner_root.path());
        let resources = resource_package(controller.path());

        let workspace = materialize_gate_workspace(
            RUNNER,
            &resources,
            crate::RunnerWorkspaceSyncMode::Snapshot,
            RUN_ID,
        )
        .expect("materialize gate workspace");
        let remote_path = workspace.sync.remote_path.clone();
        drop(workspace);

        assert!(
            !Path::new(&remote_path).exists(),
            "no runner job ever owned this workspace"
        );
    });
}
