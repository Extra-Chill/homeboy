use std::fs;
use std::path::Path;

use crate::core::runner::workspace::sync::{prune_workspaces, sync_workspace};
use crate::core::runner::workspace::types::{
    RunnerWorkspacePruneOptions, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

#[test]
fn prune_workspaces_previews_orphans_without_deleting_by_default() {
    crate::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("orphan-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "hello\n").expect("source file");
        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-prune-preview","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (synced, _) = sync_workspace(
            "lab-local-prune-preview",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        fs::remove_dir_all(&source).expect("remove source");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-preview",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1);
        assert_eq!(output.candidates[0].remote_path, synced.remote_path);
        assert_eq!(output.candidates[0].reason, "source_path_missing");
        assert!(Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_apply_removes_only_metadata_backed_orphans() {
    crate::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let orphan_source = source_parent.path().join("orphan-source");
        let live_source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&orphan_source).expect("orphan source dir");
        fs::create_dir_all(&live_source).expect("live source dir");
        fs::write(orphan_source.join("file.txt"), "orphan\n").expect("orphan file");
        fs::write(live_source.join("file.txt"), "live\n").expect("live file");
        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-prune-apply","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (orphan, _) = sync_workspace(
            "lab-local-prune-apply",
            sync_options(orphan_source.display().to_string()),
        )
        .expect("sync orphan workspace");
        let (live, _) = sync_workspace(
            "lab-local-prune-apply",
            sync_options(live_source.display().to_string()),
        )
        .expect("sync live workspace");
        let unmanaged = runner_root
            .path()
            .join("_lab_workspaces")
            .join("unmanaged-old-workspace");
        fs::create_dir_all(&unmanaged).expect("unmanaged workspace");
        fs::write(unmanaged.join("file.txt"), "do not delete\n").expect("unmanaged file");
        fs::remove_dir_all(&orphan_source).expect("remove orphan source");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-apply",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
            },
        )
        .expect("prune apply");

        assert_eq!(exit_code, 0);
        assert!(!output.dry_run);
        assert_eq!(output.removed.len(), 1);
        assert_eq!(output.removed[0].remote_path, orphan.remote_path);
        assert!(!Path::new(&orphan.remote_path).exists());
        assert!(Path::new(&live.remote_path).exists());
        assert!(unmanaged.exists());
    });
}

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
