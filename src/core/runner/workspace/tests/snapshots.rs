use std::fs;

use super::git;
use crate::core::runner::workspace::sync::{sync_workspace, workspace_snapshots};
use crate::core::runner::workspace::types::{
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

#[test]
fn workspace_snapshots_render_metadata_for_synced_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("blocks-engine@figma-fixture");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test User"]);
        git(
            &source,
            &["checkout", "-b", "run/figma-matrix-refreshed-20260627"],
        );
        fs::write(source.join("fixture.txt"), "figma\n").expect("fixture");
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "fixture"]);
        let commit =
            crate::core::runner::workspace::util::git_output(&source, &["rev-parse", "HEAD"])
                .expect("commit");

        create_local_runner("lab-local-snapshots", runner_root.path());
        let (synced, _) = sync_workspace(
            "lab-local-snapshots",
            sync_options(
                source.display().to_string(),
                Some("run-figma-1".to_string()),
            ),
        )
        .expect("sync workspace");

        let (output, exit_code) = workspace_snapshots(
            "lab-local-snapshots",
            RunnerWorkspaceSnapshotFilters {
                repo: Some("blocks-engine".to_string()),
                source_ref: Some("run/figma-matrix-refreshed-20260627".to_string()),
                source_commit: Some(commit.clone()),
                run_id: Some("run-figma-1".to_string()),
                limit: 10,
            },
        )
        .expect("snapshots");

        assert_eq!(exit_code, 0);
        assert_eq!(output.variant, "workspace_snapshots");
        assert_eq!(output.command, "runner.workspace.snapshots");
        assert_eq!(output.filters.repo.as_deref(), Some("blocks-engine"));
        assert_eq!(output.snapshots.len(), 1);
        let snapshot = &output.snapshots[0];
        assert_eq!(snapshot.runner_id, "lab-local-snapshots");
        assert_eq!(snapshot.repo, "blocks-engine");
        assert_eq!(snapshot.local_path, synced.local_path);
        assert_eq!(snapshot.remote_path, synced.remote_path);
        assert_eq!(snapshot.sync_mode, "snapshot");
        assert_eq!(snapshot.snapshot_identity, synced.snapshot_identity);
        assert_eq!(
            snapshot.source_ref.as_deref(),
            Some("run/figma-matrix-refreshed-20260627")
        );
        assert_eq!(snapshot.source_commit.as_deref(), Some(commit.as_str()));
        assert_eq!(snapshot.source_dirty, Some(false));
        assert_eq!(snapshot.run_id.as_deref(), Some("run-figma-1"));
        assert!(snapshot.created_at.contains('T'));
        assert!(snapshot
            .exec_command
            .contains("homeboy runner exec lab-local-snapshots --cwd"));
    });
}

#[test]
fn workspace_snapshots_filters_by_repo_ref_commit_and_run() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-filter", runner_root.path());
        let first = git_source("homeboy@one", "feature/one", "one\n");
        let second = git_source("blocks-engine@two", "feature/two", "two\n");
        let first_commit =
            crate::core::runner::workspace::util::git_output(first.path(), &["rev-parse", "HEAD"])
                .expect("first commit");

        sync_workspace(
            "lab-local-filter",
            sync_options(
                first.path().display().to_string(),
                Some("run-one".to_string()),
            ),
        )
        .expect("sync first");
        sync_workspace(
            "lab-local-filter",
            sync_options(
                second.path().display().to_string(),
                Some("run-two".to_string()),
            ),
        )
        .expect("sync second");

        let (output, _) = workspace_snapshots(
            "lab-local-filter",
            RunnerWorkspaceSnapshotFilters {
                repo: Some("homeboy".to_string()),
                source_ref: Some("feature/one".to_string()),
                source_commit: Some(first_commit),
                run_id: Some("run-one".to_string()),
                limit: 10,
            },
        )
        .expect("filtered snapshots");

        assert_eq!(output.snapshots.len(), 1);
        assert_eq!(output.snapshots[0].repo, "homeboy");
        assert_eq!(output.snapshots[0].run_id.as_deref(), Some("run-one"));

        let (none, _) = workspace_snapshots(
            "lab-local-filter",
            RunnerWorkspaceSnapshotFilters {
                repo: Some("homeboy".to_string()),
                source_ref: Some("feature/two".to_string()),
                source_commit: None,
                run_id: None,
                limit: 10,
            },
        )
        .expect("mismatched filter");
        assert!(none.snapshots.is_empty());
    });
}

fn create_local_runner(id: &str, workspace_root: &std::path::Path) {
    crate::core::runner::create(
        &format!(
            r#"{{"id":"{}","kind":"local","workspace_root":"{}"}}"#,
            id,
            workspace_root.display()
        ),
        false,
    )
    .expect("create runner");
}

fn sync_options(path: String, run_id: Option<String>) -> RunnerWorkspaceSyncOptions {
    RunnerWorkspaceSyncOptions {
        path,
        mode: RunnerWorkspaceSyncMode::Snapshot,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        run_isolation_token: run_id,
    }
}

fn git_source(name: &str, branch: &str, content: &str) -> tempfile::TempDir {
    let source = tempfile::Builder::new()
        .prefix(name)
        .tempdir()
        .expect("source tempdir");
    git(source.path(), &["init"]);
    git(source.path(), &["config", "user.email", "test@example.com"]);
    git(source.path(), &["config", "user.name", "Test User"]);
    git(source.path(), &["checkout", "-b", branch]);
    fs::write(source.path().join("file.txt"), content).expect("source file");
    git(source.path(), &["add", "."]);
    git(source.path(), &["commit", "-m", "base"]);
    source
}
