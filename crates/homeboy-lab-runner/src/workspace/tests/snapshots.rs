use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use super::git;
use crate::workspace::sync::workspace_metadata_file;
use crate::workspace::sync::{
    prepared_source_cache_command, prepared_source_view_command, save_prepared_source_cache,
    sync_workspace, workspace_snapshot_scan_command, workspace_snapshots,
};
use crate::workspace::types::{
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use homeboy_lab_contract::lab::handoff::runner_workspace_prepared_source_marker_path;

static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(unix)]
fn make_cache_fixture_writable(root: &std::path::Path) {
    let status = Command::new("chmod")
        .args(["-R", "u+w"])
        .arg(root)
        .status()
        .expect("restore fixture permissions");
    assert!(status.success(), "restore fixture permissions");
}

#[cfg(not(unix))]
fn make_cache_fixture_writable(root: &std::path::Path) {
    fn restore(path: &std::path::Path) -> std::io::Result<()> {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
        if path.is_dir() {
            for entry in std::fs::read_dir(path)? {
                restore(&entry?.path())?;
            }
        }
        Ok(())
    }

    restore(root).expect("restore fixture permissions");
}

#[test]
fn workspace_snapshot_scan_skips_a_child_removed_during_atomic_promotion() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    let root = tempfile::tempdir().expect("workspace root");
    let temporary = root.path().join("workspace.tmp-promoting");
    fs::create_dir_all(&temporary).expect("temporary workspace");
    fs::create_dir_all(workspace_metadata_file(&temporary).parent().unwrap())
        .expect("temporary workspace state");
    fs::write(
        workspace_metadata_file(&temporary),
        r#"{"schema":"homeboy/runner-workspace/v1"}"#,
    )
    .expect("temporary metadata");
    let shim_root = tempfile::tempdir().expect("base64 shim root");
    let shim = shim_root.path().join("base64");
    homeboy_test_support::write_executable(
        &shim,
        format!(
            "#!/bin/sh\nrm -rf -- {}\nexit 1\n",
            shell_quote(temporary.as_path())
        ),
        0o755,
    )
    .expect("base64 shim");

    let original_path = std::env::var_os("PATH").expect("PATH");
    let mut paths = vec![shim_root.path().to_path_buf()];
    paths.extend(std::env::split_paths(&original_path));
    // Scope the fault-injection base64 to this scan, never sibling tests.
    let output = std::process::Command::new("sh")
        .env("PATH", std::env::join_paths(paths).expect("PATH value"))
        .args([
            "-c",
            &workspace_snapshot_scan_command(&root.path().display().to_string()),
        ])
        .output()
        .expect("run snapshot scan");

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
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(workspace_metadata_file(&workspace).parent().unwrap())
        .expect("workspace state");
    fs::write(workspace_metadata_file(&workspace), "{}").expect("workspace metadata");
    let shim_root = tempfile::tempdir().expect("base64 shim root");
    let shim = shim_root.path().join("base64");
    homeboy_test_support::write_executable(
        &shim,
        format!(
            "#!/bin/sh\nrm -rf -- {}\nexit 1\n",
            shell_quote(root.path())
        ),
        0o755,
    )
    .expect("base64 shim");

    let original_path = std::env::var_os("PATH").expect("PATH");
    let mut paths = vec![shim_root.path().to_path_buf()];
    paths.extend(std::env::split_paths(&original_path));
    let output = std::process::Command::new("sh")
        .env("PATH", std::env::join_paths(paths).expect("PATH value"))
        .args([
            "-c",
            &workspace_snapshot_scan_command(&root.path().display().to_string()),
        ])
        .output()
        .expect("run snapshot scan");

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
            .contains("homeboy runner exec --workspace-ref"));
    });
}

#[test]
fn stale_truncated_metadata_does_not_block_current_workspace_sync() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-stale-metadata", runner_root.path());
        let stale_checkout = runner_root.path().join("_lab_workspaces/stale-unrelated");
        fs::create_dir_all(&stale_checkout).expect("stale checkout");
        let stale_metadata = workspace_metadata_file(&stale_checkout);
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
        assert_eq!(diagnostic.field, "runner-workspace.json");
        assert!(diagnostic.error.contains("EOF while parsing a string"));
        assert!(diagnostic.error.len() <= 512 + "... [truncated]".len());
    });
}

#[test]
fn ssh_snapshot_listing_preserves_entries_and_isolates_invalid_metadata() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_ssh_runner("lab-ssh-snapshots", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "source\n").expect("source");
        let (synced, _) = sync_workspace(
            "lab-ssh-snapshots",
            sync_options(
                source.path().display().to_string(),
                Some("ssh-run".to_string()),
            ),
        )
        .expect("sync");
        let invalid_checkout = runner_root.path().join("_lab_workspaces/invalid");
        fs::create_dir_all(&invalid_checkout).expect("invalid checkout");
        let invalid = workspace_metadata_file(&invalid_checkout);
        fs::create_dir_all(invalid.parent().expect("invalid parent")).expect("invalid workspace");
        fs::write(&invalid, "{\"workspace_lease\":").expect("invalid metadata");

        let (output, _) = workspace_snapshots(
            "lab-ssh-snapshots",
            RunnerWorkspaceSnapshotFilters {
                run_id: Some("ssh-run".to_string()),
                limit: 10,
                ..Default::default()
            },
        )
        .expect("SSH snapshot listing");

        assert_eq!(output.variant, "workspace_snapshots");
        assert_eq!(output.snapshots.len(), 1);
        assert_eq!(output.snapshots[0].remote_path, synced.remote_path);
        assert_eq!(output.skipped_invalid_metadata.len(), 1);
        // The SSH scan names the checkout whose state metadata is invalid;
        // the metadata itself lives in the sibling `-homeboy-state` directory.
        assert_eq!(
            output.skipped_invalid_metadata[0].source,
            invalid_checkout.display().to_string()
        );
    });
}

#[test]
fn workspace_snapshots_filters_by_repo_ref_commit_and_run() {
    crate::test_support::with_isolated_home(|_| {
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
fn same_repository_and_commit_share_prepared_source_cache_across_worktrees() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-prepared-cache-worktree", runner_root.path());
        let source = git_source(
            "homeboy@prepared-cache-worktree",
            "fix/prepared-cache",
            "source\n",
        );
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/prepared-cache-worktree.git",
            ],
        );
        // A second wave child at the identical commit, materializing from an
        // independent worktree of the same repository rather than the first
        // child's checkout path.
        let second_attempt_parent = tempfile::tempdir().expect("second attempt parent");
        let second_attempt_path = second_attempt_parent.path().join("workspace");
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                second_attempt_path.to_str().expect("second attempt path"),
                "HEAD",
            ],
        );

        let mut options = sync_options(
            source.path().display().to_string(),
            Some("job-one".to_string()),
        );
        options.mode = RunnerWorkspaceSyncMode::Git;
        options.controller_routed_git = true;
        let (first, _) = sync_workspace("lab-local-prepared-cache-worktree", options.clone())
            .expect("first child materializes source");
        std::fs::write(
            std::path::Path::new(&first.remote_path).join("dependency-marker"),
            "hydrated\n",
        )
        .expect("simulate dependency hydration");
        save_prepared_source_cache(
            "lab-local-prepared-cache-worktree",
            &first.local_path,
            &first.remote_path,
        )
        .expect("save immutable prepared source");

        options.path = second_attempt_path.display().to_string();
        options.run_isolation_token = Some("job-two".to_string());
        let (second, _) = sync_workspace("lab-local-prepared-cache-worktree", options)
            .expect("second child materializes from its own worktree");

        assert_ne!(first.remote_path, second.remote_path);
        assert_eq!(
            second
                .materialization_plan
                .actual_materialization_mode
                .as_deref(),
            Some("prepared_source_view"),
            "a sibling worktree of the same repository at the identical commit must reuse \
             the already-hydrated prepared source instead of hydrating again"
        );
        assert!(
            std::path::Path::new(&second.remote_path)
                .join("dependency-marker")
                .is_file(),
            "the reused view must carry the first child's completed hydration"
        );
        make_cache_fixture_writable(&runner_root.path().join("_lab_prepared_sources"));
    });
}

#[test]
fn same_commit_prepared_source_cache_creates_private_job_views() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-prepared-cache", runner_root.path());
        let source = git_source("homeboy@prepared-cache", "fix/prepared-cache", "source\n");
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/prepared-cache.git",
            ],
        );
        let mut options = sync_options(
            source.path().display().to_string(),
            Some("job-one".to_string()),
        );
        options.mode = RunnerWorkspaceSyncMode::Git;
        options.controller_routed_git = true;
        let (first, _) = sync_workspace("lab-local-prepared-cache", options.clone())
            .expect("first job materializes source");
        std::fs::write(
            std::path::Path::new(&first.remote_path).join("dependency-marker"),
            "hydrated\n",
        )
        .expect("simulate dependency hydration");
        let lifecycle = save_prepared_source_cache(
            "lab-local-prepared-cache",
            &first.local_path,
            &first.remote_path,
        )
        .expect("save immutable prepared source");
        assert!(lifecycle
            .observations
            .iter()
            .any(|entry| entry.contains("event=created")));

        options.run_isolation_token = Some("job-two".to_string());
        let (second, _) = sync_workspace("lab-local-prepared-cache", options)
            .expect("same commit receives private cached view");

        assert_ne!(first.remote_path, second.remote_path);
        assert_eq!(
            second
                .materialization_plan
                .actual_materialization_mode
                .as_deref(),
            Some("prepared_source_view")
        );
        assert!(second.materialization_plan.controller_git_bundle.is_none());
        let second_metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(workspace_metadata_file(&second.remote_path))
                .expect("second workspace metadata"),
        )
        .expect("parse second workspace metadata");
        assert_eq!(
            second_metadata["actual_materialization_mode"],
            "prepared_source_view"
        );
        assert!(std::path::Path::new(&second.remote_path)
            .join("dependency-marker")
            .is_file());
        std::fs::write(
            std::path::Path::new(&second.remote_path).join("job-only"),
            "private\n",
        )
        .expect("mutate private job view");
        assert!(!std::path::Path::new(&first.remote_path)
            .join("job-only")
            .exists());
        assert!(runner_root
            .path()
            .join("_lab_prepared_sources")
            .read_dir()
            .expect("prepared cache directory")
            .next()
            .is_some());
        make_cache_fixture_writable(&runner_root.path().join("_lab_prepared_sources"));
    });
}

#[test]
fn prepared_source_cache_retains_a_bounded_completed_working_set() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lab-local-prepared-cache-retention", runner_root.path());
        let source = git_source(
            "homeboy@prepared-cache-retention",
            "fix/prepared-cache",
            "one\n",
        );
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/prepared-cache-retention.git",
            ],
        );
        let mut options = sync_options(
            source.path().display().to_string(),
            Some("job-0".to_string()),
        );
        options.mode = RunnerWorkspaceSyncMode::Git;
        options.controller_routed_git = true;

        for index in 0..=8 {
            fs::write(source.path().join("file.txt"), format!("source-{index}\n"))
                .expect("update source");
            git(source.path(), &["add", "."]);
            git(source.path(), &["commit", "-m", &format!("source {index}")]);
            options.run_isolation_token = Some(format!("job-{index}"));
            let (synced, _) = sync_workspace("lab-local-prepared-cache-retention", options.clone())
                .expect("materialize source");
            save_prepared_source_cache(
                "lab-local-prepared-cache-retention",
                &synced.local_path,
                &synced.remote_path,
            )
            .expect("save prepared cache");
        }

        let cache_root = runner_root.path().join("_lab_prepared_sources");
        let completed = cache_root
            .read_dir()
            .expect("prepared cache root")
            .filter_map(Result::ok)
            .filter(|entry| {
                Path::new(&runner_workspace_prepared_source_marker_path(
                    &entry.path().display().to_string(),
                ))
                .is_file()
            })
            .count();
        assert_eq!(completed, 8);
        for entry in cache_root
            .read_dir()
            .expect("prepared cache root")
            .flatten()
        {
            assert!(
                !entry.path().join(".homeboy").exists(),
                "prepared source cache entries carry no in-tree Homeboy state"
            );
        }
        make_cache_fixture_writable(&cache_root);
    });
}

#[test]
#[cfg(unix)]
fn prepared_source_cache_uses_one_lock_for_views_and_pruning() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    let root = tempfile::tempdir().expect("cache root");
    let cache_root = root.path().join("_lab_prepared_sources");
    let cache = cache_root.join("cache-locked");
    fs::create_dir_all(&cache).expect("cache");
    write_prepared_source_marker(&cache);
    fs::write(cache.join("marker"), "cached\n").expect("cache marker");
    for index in 0..8 {
        let entry = cache_root.join(format!("cache-new-{index}"));
        fs::create_dir_all(&entry).expect("newer cache");
        write_prepared_source_marker(&entry);
    }
    let tools = tempfile::tempdir().expect("tool shims");
    let started = root.path().join("copy-started");
    let release = root.path().join("copy-release");
    let cp = tools.path().join("cp");
    homeboy_test_support::write_executable(&cp, "#!/bin/sh\n: > \"$HOMEBOY_COPY_STARTED\"\nwhile [ ! -f \"$HOMEBOY_COPY_RELEASE\" ]; do sleep 0.01; done\nexec /bin/cp \"$@\"\n", 0o755).expect("cp shim");

    let destination = root.path().join("view");
    let mut view = Command::new("sh")
        .args([
            "-c",
            &prepared_source_view_command(cache.to_str().unwrap(), destination.to_str().unwrap()),
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                tools.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOMEBOY_COPY_STARTED", &started)
        .env("HOMEBOY_COPY_RELEASE", &release)
        .spawn()
        .expect("start materialization");
    for _ in 0..100 {
        if started.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(started.exists(), "materialization acquired the cache lock");
    let prune = Command::new("sh")
        .args([
            "-c",
            &prepared_source_cache_command(
                cache.to_str().unwrap(),
                cache_root.to_str().unwrap(),
                cache.to_str().unwrap(),
            ),
        ])
        .output()
        .expect("run concurrent prune");
    assert!(prune.status.success(), "{prune:?}");
    assert!(cache.exists(), "prune must defer a lock-owned cache");
    fs::write(&release, "").expect("release copy");
    assert!(view.wait().expect("wait materialization").success());
    assert!(destination.join("marker").is_file());
    assert!(!destination.join(".homeboy").exists());
    assert!(Path::new(&runner_workspace_prepared_source_marker_path(
        destination.to_str().unwrap()
    ))
    .is_file());
}

fn write_prepared_source_marker(tree: &Path) {
    let marker = runner_workspace_prepared_source_marker_path(&tree.display().to_string());
    let marker = Path::new(&marker);
    fs::create_dir_all(marker.parent().expect("state dir")).expect("state dir");
    fs::write(marker, "").expect("ready marker");
}

#[test]
#[cfg(unix)]
fn prepared_source_cache_rechecks_for_a_concurrent_publisher_after_locking() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    let root = tempfile::tempdir().expect("cache root");
    let cache_root = root.path().join("_lab_prepared_sources");
    let cache = cache_root.join("shared-revision");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source");
    fs::write(source.join("marker"), "contender\n").expect("source marker");

    let tools = tempfile::tempdir().expect("tool shims");
    let mkdir = tools.path().join("mkdir");
    homeboy_test_support::write_executable(&mkdir, "#!/bin/sh\ncase \"$1\" in\n  *.lock)\n    /bin/mkdir -p \"$HOMEBOY_RACE_CACHE\" \"$HOMEBOY_RACE_CACHE-homeboy-state\"\n    printf 'winner\\n' > \"$HOMEBOY_RACE_CACHE/marker\"\n    /bin/chmod -R a-w \"$HOMEBOY_RACE_CACHE\"\n    : > \"$HOMEBOY_RACE_CACHE-homeboy-state/prepared-source-ready\"\n    ;;\nesac\nexec /bin/mkdir \"$@\"\n", 0o755)
    .expect("mkdir shim");

    let output = Command::new("sh")
        .args([
            "-c",
            &prepared_source_cache_command(
                cache.to_str().unwrap(),
                cache_root.to_str().unwrap(),
                source.to_str().unwrap(),
            ),
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                tools.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOMEBOY_RACE_CACHE", &cache)
        .output()
        .expect("run publication contender");

    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("event=hit"));
    assert_eq!(
        fs::read_to_string(cache.join("marker")).unwrap(),
        "winner\n"
    );
    make_cache_fixture_writable(&cache_root);
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

fn create_ssh_runner(id: &str, workspace_root: &std::path::Path) {
    homeboy_core::server::create(
        &format!(r#"{{"id":"{id}","host":"localhost","user":"test"}}"#),
        false,
    )
    .expect("create localhost server");
    crate::create(
        &format!(
            r#"{{"id":"{id}","kind":"ssh","server_id":"{id}","workspace_root":"{}"}}"#,
            workspace_root.display()
        ),
        false,
    )
    .expect("create SSH runner");
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
        validation_dependency_ids: None,
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
