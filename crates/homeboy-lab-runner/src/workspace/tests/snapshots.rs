use std::fs;
use std::sync::{Mutex, OnceLock};

use super::git;
use crate::workspace::snapshot::{incremental_prepare_command_fits, snapshot_manifest_delta};
use crate::workspace::sync::{
    reuse_compatible_snapshot_workspace, sync_workspace, workspace_snapshot_scan_command,
    workspace_snapshots,
};
use crate::workspace::types::{
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::workspace::{WorkspaceContentManifest, WorkspaceContentManifestEntry};

static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn workspace_snapshot_scan_skips_a_child_removed_during_atomic_promotion() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    let root = tempfile::tempdir().expect("workspace root");
    let temporary = root.path().join("workspace.tmp-promoting");
    fs::create_dir_all(temporary.join(".homeboy")).expect("temporary workspace");
    fs::write(
        temporary.join(".homeboy/runner-workspace.json"),
        r#"{"schema":"homeboy/runner-workspace/v1"}"#,
    )
    .expect("temporary metadata");
    let shim_root = tempfile::tempdir().expect("base64 shim root");
    let shim = shim_root.path().join("base64");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nrm -rf -- {}\nexit 1\n",
            shell_quote(temporary.as_path())
        ),
    )
    .expect("base64 shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
            .expect("make shim executable");
    }
    let original_path = std::env::var_os("PATH").expect("PATH");
    let mut paths = vec![shim_root.path().to_path_buf()];
    paths.extend(std::env::split_paths(&original_path));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH value"));
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            &workspace_snapshot_scan_command(&root.path().display().to_string()),
        ])
        .output()
        .expect("run snapshot scan");
    std::env::set_var("PATH", original_path);

    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "vanished temporary workspace must be skipped"
    );
    assert!(
        !temporary.exists(),
        "shim must remove the temporary workspace during scan"
    );
}

#[test]
fn workspace_snapshot_scan_fails_when_its_root_disappears() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(workspace.join(".homeboy")).expect("workspace");
    fs::write(workspace.join(".homeboy/runner-workspace.json"), "{}").expect("workspace metadata");
    let shim_root = tempfile::tempdir().expect("base64 shim root");
    let shim = shim_root.path().join("base64");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nrm -rf -- {}\nexit 1\n",
            shell_quote(root.path())
        ),
    )
    .expect("base64 shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
            .expect("make shim executable");
    }
    let original_path = std::env::var_os("PATH").expect("PATH");
    let mut paths = vec![shim_root.path().to_path_buf()];
    paths.extend(std::env::split_paths(&original_path));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH value"));
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            &workspace_snapshot_scan_command(&root.path().display().to_string()),
        ])
        .output()
        .expect("run snapshot scan");
    std::env::set_var("PATH", original_path);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("runner workspace snapshot root disappeared during scan"));
}

fn shell_quote(path: &std::path::Path) -> String {
    format!(
        "'{}'",
        path.display().to_string().replace('\'', "'\\\"'\\\"'")
    )
}

#[test]
fn workspace_snapshots_render_metadata_for_synced_workspace() {
    homeboy_core::test_support::with_isolated_home(|_| {
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
            crate::workspace::util::git_output(&source, &["rev-parse", "HEAD"]).expect("commit");

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
fn stale_truncated_metadata_does_not_block_current_workspace_sync() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-stale-metadata", runner_root.path());
        let stale_metadata = runner_root
            .path()
            .join("_lab_workspaces/stale-unrelated/.homeboy/runner-workspace.json");
        fs::create_dir_all(stale_metadata.parent().expect("metadata parent"))
            .expect("stale workspace parent");
        fs::write(&stale_metadata, "{\n  \"runner_id\": \"").expect("truncated metadata");

        let source = git_source("homeboy@current-task", "fix/current-task", "source\n");
        let (synced, _) = sync_workspace(
            "lab-local-stale-metadata",
            sync_options(
                source.path().display().to_string(),
                Some("current-run".to_string()),
            ),
        )
        .expect("current workspace sync ignores unrelated stale metadata");

        let (output, _) = workspace_snapshots(
            "lab-local-stale-metadata",
            RunnerWorkspaceSnapshotFilters {
                run_id: Some("current-run".to_string()),
                limit: 10,
                ..Default::default()
            },
        )
        .expect("current workspace snapshot discovery");

        assert_eq!(output.snapshots.len(), 1);
        assert_eq!(output.snapshots[0].remote_path, synced.remote_path);
        assert_eq!(output.skipped_invalid_metadata.len(), 1);
        let diagnostic = &output.skipped_invalid_metadata[0];
        assert_eq!(diagnostic.source, stale_metadata.display().to_string());
        assert_eq!(diagnostic.field, ".homeboy/runner-workspace.json");
        assert!(diagnostic.error.contains("EOF while parsing a string"));
        assert!(diagnostic.error.len() <= 512 + "... [truncated]".len());
    });
}

#[test]
fn workspace_snapshots_filters_by_repo_ref_commit_and_run() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-filter", runner_root.path());
        let first = git_source("homeboy@one", "feature/one", "one\n");
        let second = git_source("blocks-engine@two", "feature/two", "two\n");
        let first_commit = crate::workspace::util::git_output(first.path(), &["rev-parse", "HEAD"])
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

#[test]
fn clean_snapshot_reuse_preserves_exact_source_provenance_without_git_materialization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-reuse", runner_root.path());
        let source = git_source("homeboy@partial-clone", "fix/reuse", "source\n");
        let commit = crate::workspace::util::git_output(source.path(), &["rev-parse", "HEAD"])
            .expect("source commit");

        // Snapshot sync is intentionally independent of Git object closure
        // hydration, as a blob:none controller checkout may not hold it.
        let (synced, _) = sync_workspace(
            "lab-local-reuse",
            sync_options(source.path().display().to_string(), None),
        )
        .expect("initial snapshot sync");
        let mut retry_options = sync_options(
            source.path().display().to_string(),
            Some("retry-attempt".to_string()),
        );
        // A retry normally requests Git materialization. An exact snapshot is
        // still the stronger transport choice when no Git-only ref is needed.
        retry_options.mode = RunnerWorkspaceSyncMode::Git;
        let reused = reuse_compatible_snapshot_workspace("lab-local-reuse", &retry_options)
            .expect("look up compatible snapshot")
            .expect("clean exact snapshot is reused");

        assert_eq!(reused.remote_path, synced.remote_path);
        assert_eq!(reused.snapshot_identity, synced.snapshot_identity);
        assert_eq!(
            reused.current_workspace.source_commit.as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(reused.current_workspace.source_dirty, Some(false));
        assert_eq!(reused.sync_mode, RunnerWorkspaceSyncMode::Snapshot);
        assert_eq!(
            reused.materialization_plan.declared_inputs.mode,
            RunnerWorkspaceSyncMode::Snapshot
        );
        assert_eq!(
            reused.workspace_cleanliness,
            "snapshot_reused_clean_workspace"
        );
        assert!(reused.materialization_plan.controller_git_bundle.is_none());
    });
}

#[test]
fn incremental_snapshots_reuse_unchanged_content_and_reconcile_deltas() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-incremental", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::create_dir_all(source.path().join("src")).expect("source directory");
        fs::write(source.path().join("src/unchanged.txt"), "unchanged\n").expect("unchanged");
        fs::write(source.path().join("src/changed.txt"), "first\n").expect("changed");
        fs::write(source.path().join("removed.txt"), "remove\n").expect("removed");

        let (first, _) = sync_workspace(
            "lab-local-incremental",
            sync_options(
                source.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("first snapshot");
        fs::write(source.path().join("src/changed.txt"), "second\n").expect("small delta");
        fs::remove_file(source.path().join("removed.txt")).expect("delete source file");
        let (second, _) = sync_workspace(
            "lab-local-incremental",
            sync_options(
                source.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("incremental snapshot");

        let transfer = second
            .materialization_plan
            .snapshot_transfer
            .expect("transfer accounting");
        assert_eq!(transfer.transferred.files, 1);
        assert_eq!(transfer.reused.files, 1);
        assert_eq!(
            fs::read_to_string(format!("{}/src/changed.txt", second.remote_path)).unwrap(),
            "second\n"
        );
        assert!(!std::path::Path::new(&second.remote_path)
            .join("removed.txt")
            .exists());
        assert_eq!(
            fs::read_to_string(format!("{}/src/changed.txt", first.remote_path)).unwrap(),
            "first\n"
        );
    });
}

#[test]
fn incremental_snapshots_materialize_unchanged_content_without_transfer() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-unchanged", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("unchanged.txt"), "unchanged\n").expect("source file");
        sync_workspace(
            "lab-local-unchanged",
            sync_options(
                source.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("first snapshot");

        let (second, _) = sync_workspace(
            "lab-local-unchanged",
            sync_options(
                source.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("unchanged incremental snapshot");

        let transfer = second
            .materialization_plan
            .snapshot_transfer
            .expect("transfer accounting");
        assert_eq!(transfer.transferred.files, 0);
        assert_eq!(transfer.reused.files, 1);
        assert_eq!(transfer.final_size.files, 1);
        assert_eq!(
            fs::read_to_string(std::path::Path::new(&second.remote_path).join("unchanged.txt"))
                .expect("reused file"),
            "unchanged\n"
        );
    });
}

#[test]
fn incremental_snapshot_falls_back_to_full_transport_for_large_compatible_seed() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-large-incremental", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::create_dir_all(source.path().join("files")).expect("source directory");
        for index in 0..2_254 {
            fs::write(
                source.path().join(format!("files/retained-{index:04}.txt")),
                format!("seed-{index}\n"),
            )
            .expect("seed file");
        }
        sync_workspace(
            "lab-local-large-incremental",
            sync_options(
                source.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("large seed snapshot");

        fs::write(source.path().join("files/retained-0000.txt"), "updated\n").expect("small delta");
        let (second, _) = sync_workspace(
            "lab-local-large-incremental",
            sync_options(
                source.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("large compatible snapshot");

        let transfer = second
            .materialization_plan
            .snapshot_transfer
            .expect("transfer accounting");
        assert_eq!(transfer.reused.files, 0);
        assert_eq!(transfer.transferred.files, 2_254);
        assert_eq!(transfer.final_size.files, 2_254);
        assert_eq!(
            fs::read_to_string(
                std::path::Path::new(&second.remote_path).join("files/retained-0000.txt")
            )
            .expect("updated file"),
            "updated\n"
        );
        assert_eq!(
            fs::read_to_string(
                std::path::Path::new(&second.remote_path).join("files/retained-2253.txt")
            )
            .expect("retained file"),
            "seed-2253\n"
        );
        for index in 0..2_254 {
            let expected = if index == 0 {
                "updated\n".to_string()
            } else {
                format!("seed-{index}\n")
            };
            assert_eq!(
                fs::read_to_string(
                    std::path::Path::new(&second.remote_path)
                        .join(format!("files/retained-{index:04}.txt"))
                )
                .expect("exact fallback content"),
                expected
            );
        }
    });
}

#[test]
fn incremental_prepare_preflight_rejects_quote_heavy_manifest_before_command_construction() {
    let entry = |index| WorkspaceContentManifestEntry {
        path: format!("files/{}-{index:04}.txt", "'".repeat(64)),
        kind: "file".to_string(),
        sha256: Some(format!("sha256:{}", "0".repeat(64))),
        bytes: Some(1),
        owner_executable: None,
    };
    let manifest = WorkspaceContentManifest {
        entry_count: 256,
        entries: (0..256).map(entry).collect(),
    };
    let delta = snapshot_manifest_delta(&manifest, &manifest).expect("valid manifest delta");

    assert!(
        !incremental_prepare_command_fits(
            "/runner/workspace/with a deliberately long remote path",
            "/runner/workspace/with a deliberately long remote path.tmp-12345678",
            "/runner/seed/with a deliberately long path",
            &delta,
        ),
        "quote-heavy paths must select full transport before building the nested shell command"
    );
}

#[test]
fn incremental_snapshot_refuses_seed_when_effective_excludes_change() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-exclude-change", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("kept.txt"), "kept\n").expect("kept");
        fs::write(source.path().join("secret.txt"), "secret\n").expect("secret");
        sync_workspace(
            "lab-local-exclude-change",
            sync_options(
                source.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("first snapshot");
        // A runner policy change changes effective snapshot filtering, so the
        // prior tree is not a compatible source for hard-link reuse.
        crate::merge(
            Some("lab-local-exclude-change"),
            r#"{"policy":{"snapshot_excludes":["secret.txt"]}}"#,
            &["policy".to_string()],
        )
        .expect("update runner");
        let (second, _) = sync_workspace(
            "lab-local-exclude-change",
            sync_options(
                source.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("filtered snapshot");
        let transfer = second
            .materialization_plan
            .snapshot_transfer
            .expect("transfer");
        assert_eq!(transfer.reused.files, 0);
        assert_eq!(transfer.transferred.files, 1);
        assert!(!std::path::Path::new(&second.remote_path)
            .join("secret.txt")
            .exists());
    });
}

#[test]
fn incremental_snapshot_falls_back_when_seed_manifest_is_corrupt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-corrupt-manifest", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "contents\n").expect("source file");
        let (first, _) = sync_workspace(
            "lab-local-corrupt-manifest",
            sync_options(
                source.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("first snapshot");
        let metadata_path =
            std::path::Path::new(&first.remote_path).join(".homeboy/runner-workspace.json");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                .expect("metadata JSON");
        metadata["content_manifest"]["entry_count"] = serde_json::json!(999);
        fs::write(&metadata_path, metadata.to_string()).expect("corrupt manifest");

        let (second, _) = sync_workspace(
            "lab-local-corrupt-manifest",
            sync_options(
                source.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("safe full fallback");
        let transfer = second
            .materialization_plan
            .snapshot_transfer
            .expect("transfer accounting");
        assert_eq!(transfer.reused.files, 0);
        assert_eq!(transfer.transferred.files, 1);
        assert_eq!(
            fs::read_to_string(std::path::Path::new(&second.remote_path).join("file.txt"))
                .expect("fallback file"),
            "contents\n"
        );
    });
}

#[test]
fn incremental_snapshot_refuses_seed_from_another_source_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-incompatible", runner_root.path());
        let first = tempfile::tempdir().expect("first source");
        let second = tempfile::tempdir().expect("second source");
        fs::write(first.path().join("file.txt"), "first\n").expect("first file");
        fs::write(second.path().join("file.txt"), "second\n").expect("second file");
        sync_workspace(
            "lab-local-incompatible",
            sync_options(
                first.path().display().to_string(),
                Some("first".to_string()),
            ),
        )
        .expect("first snapshot");
        let (synced, _) = sync_workspace(
            "lab-local-incompatible",
            sync_options(
                second.path().display().to_string(),
                Some("second".to_string()),
            ),
        )
        .expect("second snapshot");
        assert_eq!(
            synced
                .materialization_plan
                .snapshot_transfer
                .expect("transfer")
                .reused
                .files,
            0
        );
    });
}

fn create_local_runner(id: &str, workspace_root: &std::path::Path) {
    crate::create(
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
