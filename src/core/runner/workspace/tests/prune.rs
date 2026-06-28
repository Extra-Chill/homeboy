use std::fs;
use std::path::Path;
use std::process::Command;

use crate::core::runner::workspace::sync::{prune_scan_command, prune_workspaces, sync_workspace};
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

#[test]
fn prune_workspaces_preview_reports_synthetic_odd_path_without_deleting() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root
            .path()
            .join("_lab_workspaces")
            .join("repo's odd (name) with spaces");
        fs::create_dir_all(workspace.join(".homeboy")).expect("workspace metadata dir");
        fs::write(workspace.join("file.txt"), "orphan\n").expect("workspace file");
        fs::write(
            workspace.join(".homeboy/runner-workspace.json"),
            serde_json::json!({
                "schema": "homeboy/runner-workspace/v1",
                "runner_id": "lab-local-prune-odd-preview",
                "local_path": runner_root.path().join("missing source's (odd) path").display().to_string(),
                "remote_path": workspace.display().to_string(),
                "sync_mode": "snapshot",
                "snapshot_identity": "synthetic",
                "synced_at": "2026-06-28T00:00:00Z"
            })
            .to_string(),
        )
        .expect("write metadata");
        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-prune-odd-preview","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-odd-preview",
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
        assert_eq!(
            output.candidates[0].remote_path,
            workspace.display().to_string()
        );
        assert!(output.candidates[0]
            .source_path
            .contains("missing source's (odd) path"));
        assert_eq!(output.candidates[0].reason, "source_path_missing");
        assert!(workspace.exists());
        assert!(output.removed.is_empty());
    });
}

#[test]
fn ssh_prune_scan_command_handles_paths_that_need_shell_quoting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root's (quoted) workspaces");
    let workspace = root.join("repo's odd (name) with spaces");
    fs::create_dir_all(workspace.join(".homeboy")).expect("workspace metadata dir");
    fs::write(workspace.join("file.txt"), "orphan\n").expect("workspace file");
    fs::write(
        workspace.join(".homeboy/runner-workspace.json"),
        serde_json::json!({
            "schema": "homeboy/runner-workspace/v1",
            "runner_id": "lab-ssh-prune-odd-scan",
            "local_path": "/missing/source's (odd) path",
            "remote_path": workspace.display().to_string(),
            "sync_mode": "snapshot",
            "snapshot_identity": "synthetic",
            "synced_at": "2026-06-28T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write metadata");

    let output = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(&root.display().to_string(), 0))
        .output()
        .expect("run generated prune scan command");

    assert!(
        output.status.success(),
        "scan command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&workspace.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains('\t'), "{stdout}");
    assert!(stdout.lines().count() == 1, "{stdout}");
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
