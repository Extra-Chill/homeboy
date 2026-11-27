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
            &crate::WorkspaceControl::default(),
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
            &crate::WorkspaceControl::default(),
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
fn workspace_snapshots_filter_by_repo_ref_commit_and_run_and_render_metadata() {
    crate::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        create_local_runner("lab-local-filter", runner_root.path());
        let first = git_source(
            "blocks-engine@figma-fixture",
            "run/figma-matrix-refreshed-20260627",
            "one\n",
        );
        let second = git_source("homeboy@two", "feature/two", "two\n");
        let first_commit = crate::workspace::util::git_output(first.path(), &["rev-parse", "HEAD"])
            .expect("first commit");

        let (synced, _) = sync_workspace(
            "lab-local-filter",
            sync_options(
                first.path().display().to_string(),
                Some("run-figma-1".to_string()),
            ),
            &crate::WorkspaceControl::default(),
        )
        .expect("sync first");
        sync_workspace(
            "lab-local-filter",
            sync_options(
                second.path().display().to_string(),
                Some("run-two".to_string()),
            ),
            &crate::WorkspaceControl::default(),
        )
        .expect("sync second");

        let (output, exit_code) = workspace_snapshots(
            "lab-local-filter",
            RunnerWorkspaceSnapshotFilters {
                repo: Some("blocks-engine".to_string()),
                source_ref: Some("run/figma-matrix-refreshed-20260627".to_string()),
                source_commit: Some(first_commit.clone()),
                run_id: Some("run-figma-1".to_string()),
                limit: 10,
            },
        )
        .expect("filtered snapshots");

        assert_eq!(exit_code, 0);
        assert_eq!(output.variant, "workspace_snapshots");
        assert_eq!(output.command, "runner.workspace.snapshots");
        assert_eq!(output.filters.repo.as_deref(), Some("blocks-engine"));
        assert_eq!(output.snapshots.len(), 1);
        let snapshot = &output.snapshots[0];
        assert_eq!(snapshot.runner_id, "lab-local-filter");
        assert_eq!(snapshot.repo, "blocks-engine");
        assert_eq!(snapshot.local_path, synced.local_path);
        assert_eq!(snapshot.remote_path, synced.remote_path);
        assert_eq!(snapshot.sync_mode, "snapshot");
        assert_eq!(snapshot.snapshot_identity, synced.snapshot_identity);
        assert_eq!(
            snapshot.source_ref.as_deref(),
            Some("run/figma-matrix-refreshed-20260627")
        );
        assert_eq!(
            snapshot.source_commit.as_deref(),
            Some(first_commit.as_str())
        );
        assert_eq!(snapshot.source_dirty, Some(false));
        assert_eq!(snapshot.run_id.as_deref(), Some("run-figma-1"));
        assert!(snapshot.created_at.contains('T'));
        assert!(snapshot
            .exec_command
            .contains("homeboy runner exec --workspace-ref"));

        let (none, _) = workspace_snapshots(
            "lab-local-filter",
            RunnerWorkspaceSnapshotFilters {
                repo: Some("blocks-engine".to_string()),
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
fn same_commit_prepared_source_cache_creates_private_job_views_across_worktrees() {
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
        // The second job materializes from an independent worktree of the same
        // repository at the identical commit, not from the first job's path.
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
        let (first, _) = sync_workspace(
            "lab-local-prepared-cache",
            options.clone(),
            &crate::WorkspaceControl::default(),
        )
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

        options.path = second_attempt_path.display().to_string();
        options.run_isolation_token = Some("job-two".to_string());
        let (second, _) = sync_workspace(
            "lab-local-prepared-cache",
            options,
            &crate::WorkspaceControl::default(),
        )
        .expect("same commit from a sibling worktree receives private cached view");

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
            let (synced, _) = sync_workspace(
                "lab-local-prepared-cache-retention",
                options.clone(),
                &crate::WorkspaceControl::default(),
            )
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
    homeboy_test_support::write_executable(&cp, "#!/bin/sh\n: > \"$HOMEBOY_COPY_STARTED\"\nn=0; while [ ! -f \"$HOMEBOY_COPY_RELEASE\" ] && [ $n -lt 12000 ]; do sleep 0.01; n=$((n+1)); done\nexec /bin/cp \"$@\"\n", 0o755).expect("cp shim");

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

/// Run one prepared-source publisher for `cache` with `tools` prepended to
/// PATH. `HOMEBOY_RACE_CACHE`/`HOMEBOY_RACE_MARKER` let tool shims publish a
/// competing entry at a precise point of the publication.
#[cfg(unix)]
fn run_prepared_source_publisher(
    cache: &Path,
    cache_root: &Path,
    source: &Path,
    tools: &Path,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let marker = runner_workspace_prepared_source_marker_path(cache.to_str().unwrap());
    let mut command = Command::new("sh");
    command
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
                tools.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOMEBOY_RACE_CACHE", cache)
        .env("HOMEBOY_RACE_MARKER", &marker);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("run prepared-source publisher")
}

/// Shell snippet a tool shim runs to publish the winning entry. `$1` selects
/// whether the winner is left read-only (like a real published entry).
#[cfg(unix)]
const PUBLISH_WINNER_SHELL: &str = "publish_winner() { /bin/mkdir -p \"$HOMEBOY_RACE_CACHE\" \"$(dirname \"$HOMEBOY_RACE_MARKER\")\"; printf 'winner\\n' > \"$HOMEBOY_RACE_CACHE/marker\"; if [ \"$1\" = readonly ]; then /bin/chmod -R a-w \"$HOMEBOY_RACE_CACHE\"; fi; : > \"$HOMEBOY_RACE_MARKER\"; }\n";

#[cfg(unix)]
fn assert_single_winner_entry(cache: &Path, cache_root: &Path) {
    assert_eq!(
        fs::read_to_string(cache.join("marker")).unwrap(),
        "winner\n",
        "the existing entry is reused, not replaced"
    );
    let entries: Vec<String> = fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["marker".to_string()], "no temp copy nested");
    let leftovers: Vec<String> = fs::read_dir(cache_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp.") || name.ends_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[test]
#[cfg(unix)]
fn prepared_source_cache_publish_onto_a_concurrently_published_key_reuses_it() {
    // #16840: another writer published the key while this one copied; the
    // old `mv tmp cache` moved the copy *into* the read-only entry and failed
    // the cook with "Permission denied".
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
    homeboy_test_support::write_executable(
        &tools.path().join("cp"),
        &format!(
            "#!/bin/sh\n{PUBLISH_WINNER_SHELL}/bin/cp \"$@\" || exit $?\npublish_winner readonly\n"
        ),
        0o755,
    )
    .expect("cp shim");

    let output = run_prepared_source_publisher(&cache, &cache_root, &source, tools.path(), &[]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("event=hit") && stdout.contains("reason=concurrent_publisher"),
        "{stdout}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).trim().is_empty(),
        "{output:?}"
    );
    assert_single_winner_entry(&cache, &cache_root);
    make_cache_fixture_writable(&cache_root);
}

#[test]
#[cfg(unix)]
fn prepared_source_cache_rename_never_nests_into_an_entry_published_at_rename_time() {
    // The key appears between the existence check and the rename. With
    // no-target-directory semantics the rename refuses; with a plain `mv`
    // (no `-T` support) the copy lands inside the entry and must be removed.
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    for (mode, readonly) in [("gnu", "readonly"), ("plain", "writable")] {
        let root = tempfile::tempdir().expect("cache root");
        let cache_root = root.path().join("_lab_prepared_sources");
        let cache = cache_root.join("shared-revision");
        let source = root.path().join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("marker"), "contender\n").expect("source marker");
        let tools = tempfile::tempdir().expect("tool shims");
        homeboy_test_support::write_executable(
            &tools.path().join("mv"),
            &format!(
                "#!/bin/sh\n{PUBLISH_WINNER_SHELL}if [ \"$1\" = --help ]; then [ \"$HOMEBOY_MV_MODE\" = gnu ] && exec /bin/mv --help; exit 1; fi\nif [ \"$1\" = -T ] && [ \"$HOMEBOY_MV_MODE\" != gnu ]; then shift; fi\npublish_winner {readonly}\nexec /bin/mv \"$@\"\n"
            ),
            0o755,
        )
        .expect("mv shim");

        let output = run_prepared_source_publisher(
            &cache,
            &cache_root,
            &source,
            tools.path(),
            &[("HOMEBOY_MV_MODE", mode)],
        );

        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("reason=concurrent_publisher"),
            "{mode}: {output:?}"
        );
        assert_single_winner_entry(&cache, &cache_root);
        make_cache_fixture_writable(&cache_root);
    }
}

#[test]
#[cfg(unix)]
fn concurrent_prepared_source_publishers_for_one_key_all_succeed_with_one_entry() {
    let root = tempfile::tempdir().expect("cache root");
    let cache_root = root.path().join("_lab_prepared_sources");
    let cache = cache_root.join("shared-revision");
    let tools = tempfile::tempdir().expect("tool shims");
    // Widen the copy window so the publishers overlap.
    homeboy_test_support::write_executable(
        &tools.path().join("cp"),
        "#!/bin/sh\nsleep 0.2\nexec /bin/cp \"$@\"\n",
        0o755,
    )
    .expect("cp shim");
    let sources: Vec<_> = (0..4)
        .map(|index| {
            let source = root.path().join(format!("source-{index}"));
            fs::create_dir_all(&source).expect("source");
            fs::write(source.join("marker"), "prepared\n").expect("source marker");
            source
        })
        .collect();

    let outputs: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = sources
            .iter()
            .map(|source| {
                let (cache, cache_root, tools) = (&cache, &cache_root, tools.path());
                scope.spawn(move || {
                    run_prepared_source_publisher(cache, cache_root, source, tools, &[])
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });

    for output in &outputs {
        assert!(output.status.success(), "{output:?}");
    }
    let created = outputs
        .iter()
        .filter(|output| String::from_utf8_lossy(&output.stdout).contains("event=created"))
        .count();
    assert_eq!(
        created, 1,
        "exactly one publisher creates the entry: {outputs:?}"
    );
    assert_eq!(
        fs::read_to_string(cache.join("marker")).unwrap(),
        "prepared\n"
    );
    let entries: Vec<_> = fs::read_dir(&cache).unwrap().collect();
    assert_eq!(entries.len(), 1, "no temp copy nested in the entry");
    assert!(Path::new(&runner_workspace_prepared_source_marker_path(
        cache.to_str().unwrap()
    ))
    .is_file());
    let leftovers: Vec<String> = fs::read_dir(&cache_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp.") || name.ends_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
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
