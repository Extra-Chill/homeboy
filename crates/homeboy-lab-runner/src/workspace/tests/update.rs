use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};

use crate::workspace::sync::{sync_workspace, update_workspace};
use crate::workspace::types::{
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceUpdateOptions,
};

#[test]
fn prepared_workspace_update_applies_delta_retains_assets_and_rotates_lease() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("prepared-update", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("changed.txt"), "before\n").expect("source");
        fs::write(source.path().join("deleted.txt"), "delete\n").expect("source");
        let (synced, _) =
            sync_workspace("prepared-update", sync_options(source.path())).expect("sync");
        let lease = synced.prepared_workspace_lease.expect("prepared lease");
        let remote = Path::new(&synced.remote_path);
        fs::create_dir_all(remote.join("node_modules")).expect("prepared assets");
        fs::write(remote.join("node_modules/cache"), "keep\n").expect("prepared asset");

        fs::write(source.path().join("changed.txt"), "after\n").expect("update source");
        fs::remove_file(source.path().join("deleted.txt")).expect("delete source");
        let (updated, _) = update_workspace(
            "prepared-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: lease.clone(),
            },
        )
        .expect("update");

        assert_ne!(updated.lease, lease);
        assert_eq!(
            fs::read_to_string(remote.join("changed.txt")).unwrap(),
            "after\n"
        );
        assert!(!remote.join("deleted.txt").exists());
        assert_eq!(
            fs::read_to_string(remote.join("node_modules/cache")).unwrap(),
            "keep\n"
        );
        let metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(remote.join(".homeboy/runner-workspace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["workspace_lease"], updated.lease);
        assert_eq!(metadata["workspace_generation"], 1);
        assert_eq!(
            metadata["original_prepared_snapshot_identity"],
            updated.original_prepared_snapshot_identity
        );
        assert_eq!(
            metadata["update_lineage"],
            serde_json::json!(updated.update_lineage)
        );
        assert!(updated
            .exec_command
            .contains("HOMEBOY_PREPARED_WORKSPACE_ORIGINAL_SNAPSHOT"));
        assert!(updated
            .exec_command
            .contains("HOMEBOY_PREPARED_WORKSPACE_UPDATE_LINEAGE"));

        let error = update_workspace(
            "prepared-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease,
            },
        )
        .expect_err("consumed lease must reject replay");
        assert!(error.message.contains("opaque workspace lease"));
        assert_eq!(
            fs::read_to_string(remote.join("changed.txt")).unwrap(),
            "after\n"
        );
    });
}

#[test]
fn workspace_ref_resolution_is_exact_and_reports_typed_recovery() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("workspace-ref", runner_root.path());
        create_local_runner("other-runner", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "snapshot\n").expect("source");
        let (synced, _) =
            sync_workspace("workspace-ref", sync_options(source.path())).expect("sync");

        let resolved = crate::resolve_workspace_ref("workspace-ref", &synced.workspace_ref)
            .expect("resolve exact workspace ref");
        assert_eq!(resolved.remote_path, synced.remote_path);
        assert_eq!(resolved.local_path, synced.local_path);
        assert_eq!(
            resolved
                .source_snapshot
                .workspace_snapshot_identity
                .as_deref(),
            Some(synced.snapshot_identity.as_str())
        );

        for (runner_id, workspace_ref, reason) in [
            (
                "workspace-ref",
                "not-a-workspace-ref",
                "workspace_ref_invalid",
            ),
            (
                "workspace-ref",
                "workspace:00000000-0000-0000-0000-000000000000",
                "workspace_ref_missing",
            ),
            (
                "other-runner",
                synced.workspace_ref.as_str(),
                "workspace_ref_refused",
            ),
        ] {
            let error = crate::resolve_workspace_ref(runner_id, workspace_ref)
                .expect_err("unavailable workspace ref");
            assert_eq!(error.details["field"], "workspace_ref");
            assert_eq!(error.details["reason"], reason);
            assert_eq!(error.details["recovery"], "resync_workspace");
            assert!(error.details["tried"][0]
                .as_str()
                .is_some_and(|suggestion| suggestion.contains("runner workspace sync")));
        }
    });
}

#[cfg(target_os = "macos")]
#[test]
fn workspace_ref_resolution_accepts_macos_var_alias() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        let canonical_root = runner_root.path().canonicalize().expect("canonical root");
        let alias_root = std::path::Path::new("/").join(
            canonical_root
                .strip_prefix("/private")
                .expect("macOS temporary root below /private"),
        );
        assert_ne!(alias_root, canonical_root);
        assert_eq!(alias_root.canonicalize().unwrap(), canonical_root);
        create_local_runner("workspace-ref-var-alias", &alias_root);
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "snapshot\n").expect("source");
        let (synced, _) = sync_workspace("workspace-ref-var-alias", sync_options(source.path()))
            .expect("sync through /var alias");

        let resolved =
            crate::resolve_workspace_ref("workspace-ref-var-alias", &synced.workspace_ref)
                .expect("resolve /var snapshot through canonical /private/var parent");

        assert_eq!(resolved.remote_path, synced.remote_path);
        assert_eq!(
            Path::new(&resolved.remote_path).canonicalize().unwrap(),
            Path::new(&synced.remote_path).canonicalize().unwrap()
        );
    });
}

#[cfg(unix)]
#[test]
fn workspace_ref_resolution_does_not_follow_symlinked_snapshot() {
    use std::os::unix::fs::symlink;

    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("workspace-ref-symlink", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "snapshot\n").expect("source");
        let (synced, _) =
            sync_workspace("workspace-ref-symlink", sync_options(source.path())).expect("sync");
        let escaped_root = tempfile::tempdir().expect("escaped root");
        let escaped_snapshot = escaped_root.path().join("snapshot");
        fs::rename(&synced.remote_path, &escaped_snapshot).expect("move snapshot outside root");
        symlink(&escaped_snapshot, &synced.remote_path).expect("symlink snapshot into root");

        let error = crate::resolve_workspace_ref("workspace-ref-symlink", &synced.workspace_ref)
            .expect_err("workspace ref must not follow symlink outside its root");

        assert_eq!(error.details["reason"], "workspace_ref_missing");
        assert_eq!(error.details["recovery"], "resync_workspace");
    });
}

#[test]
fn workspace_ref_rejects_expired_source_drift_and_metadata_redirection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("workspace-ref-safety", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "snapshot\n").expect("source");
        let (synced, _) =
            sync_workspace("workspace-ref-safety", sync_options(source.path())).expect("sync");
        let resolved = crate::resolve_workspace_ref("workspace-ref-safety", &synced.workspace_ref)
            .expect("resolve workspace ref");
        fs::write(source.path().join("file.txt"), "drifted\n").expect("drift source");
        let drift = crate::verify_workspace_ref_hydration_source(&resolved)
            .expect_err("hydration source drift must fail closed");
        assert_eq!(drift.details["reason"], "workspace_ref_source_drift");

        let metadata_path = Path::new(&synced.remote_path).join(".homeboy/runner-workspace.json");
        let original = fs::read_to_string(&metadata_path).expect("metadata");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&original).expect("metadata JSON");
        metadata["resource_lifecycle"]["ttl"] = serde_json::json!("2000-01-01T00:00:00Z");
        fs::write(
            &metadata_path,
            serde_json::to_string_pretty(&metadata).expect("expired metadata"),
        )
        .expect("write expired metadata");
        let expired = crate::resolve_workspace_ref("workspace-ref-safety", &synced.workspace_ref)
            .expect_err("expired workspace ref must fail closed");
        assert_eq!(expired.details["field"], "workspace_ref");
        assert_eq!(expired.details["reason"], "workspace_ref_expired");
        assert_eq!(expired.details["recovery"], "resync_workspace");
        assert!(expired.details["tried"][0]
            .as_str()
            .is_some_and(|suggestion| suggestion.contains("runner workspace sync")));

        let mut redirected: serde_json::Value =
            serde_json::from_str(&original).expect("metadata JSON");
        redirected["remote_path"] = serde_json::json!(runner_root
            .path()
            .join("_lab_workspaces/redirected")
            .display()
            .to_string());
        fs::write(
            &metadata_path,
            serde_json::to_string_pretty(&redirected).expect("redirected metadata"),
        )
        .expect("write redirected metadata");
        let refused = crate::resolve_workspace_ref("workspace-ref-safety", &synced.workspace_ref)
            .expect_err("metadata cannot redirect workspace ref");
        assert_eq!(refused.details["reason"], "workspace_ref_refused");
    });
}

#[test]
fn ssh_prepared_workspace_update_resolves_fresh_lease_without_projecting_large_root() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_ssh_runner("prepared-update-ssh", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (synced, _) =
            sync_workspace("prepared-update-ssh", sync_options(source.path())).expect("sync");
        let lease = synced.prepared_workspace_lease.expect("lease");

        let root = runner_root.path().join("_lab_workspaces");
        let metadata_path = Path::new(&synced.remote_path).join(".homeboy/runner-workspace.json");
        let metadata = fs::read_to_string(&metadata_path).expect("metadata");
        let metadata_value: serde_json::Value =
            serde_json::from_str(&metadata).expect("valid metadata");
        fs::write(
            &metadata_path,
            serde_json::to_string(&metadata_value).expect("compact metadata"),
        )
        .expect("write compact metadata");
        for index in 0..24 {
            let path = root.join(format!("stale-large-{index:02}/.homeboy"));
            fs::create_dir_all(&path).expect("stale workspace");
            let oversized = metadata.replacen(
                "\n}",
                &format!(
                    ",\n  \"ignored_large_manifest_{index}\": \"{}\"\n}}",
                    "x".repeat(256 * 1024)
                ),
                1,
            );
            fs::write(path.join("runner-workspace.json"), oversized).expect("stale metadata");
        }

        fs::write(source.path().join("file.txt"), "after\n").expect("update source");
        let (updated, _) = update_workspace(
            "prepared-update-ssh",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease,
            },
        )
        .expect("fresh lease update despite oversized aggregate metadata");

        assert_eq!(
            fs::read_to_string(Path::new(&updated.remote_path).join("file.txt")).unwrap(),
            "after\n"
        );
    });
}

#[test]
fn prepared_workspace_update_rejects_relative_runner_root() {
    homeboy_core::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"relative-update","kind":"local","workspace_root":"relative"}"#,
            false,
        )
        .expect("create runner");
        let source = tempfile::tempdir().expect("source");

        let error = update_workspace(
            "relative-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: "workspace:unused".to_string(),
            },
        )
        .expect_err("relative workspace root must be rejected");

        assert!(error.message.contains("absolute path"));
    });
}

#[test]
fn duplicate_snapshot_identities_target_their_own_opaque_workspace_lease() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("duplicate-identities", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (first, _) = sync_workspace("duplicate-identities", sync_options(source.path()))
            .expect("first sync");
        let (second, _) = sync_workspace("duplicate-identities", sync_options(source.path()))
            .expect("second sync");
        assert_eq!(first.snapshot_identity, second.snapshot_identity);
        assert_ne!(
            first.prepared_workspace_lease,
            second.prepared_workspace_lease
        );
        fs::write(source.path().join("file.txt"), "after\n").expect("update source");

        update_workspace(
            "duplicate-identities",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: first.prepared_workspace_lease.expect("first lease"),
            },
        )
        .expect("update first workspace only");

        assert_eq!(
            fs::read_to_string(Path::new(&first.remote_path).join("file.txt")).unwrap(),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(Path::new(&second.remote_path).join("file.txt")).unwrap(),
            "before\n"
        );
    });
}

#[test]
fn prepared_workspace_update_rejects_changed_git_branch_lineage() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("lineage-update", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        super::git(source.path(), &["init"]);
        super::git(source.path(), &["config", "user.email", "test@example.com"]);
        super::git(source.path(), &["config", "user.name", "Test"]);
        super::git(source.path(), &["checkout", "-b", "one"]);
        fs::write(source.path().join("file.txt"), "one\n").expect("source");
        super::git(source.path(), &["add", "."]);
        super::git(source.path(), &["commit", "-m", "one"]);
        let (synced, _) =
            sync_workspace("lineage-update", sync_options(source.path())).expect("sync");
        super::git(source.path(), &["checkout", "-b", "two"]);
        let error = update_workspace(
            "lineage-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: synced.prepared_workspace_lease.expect("lease"),
            },
        )
        .expect_err("branch change must reject lineage");
        assert!(error
            .message
            .contains("unrelated repository or branch lineage"));
    });
}

#[test]
fn competing_prepared_workspace_updates_allow_exactly_one_promotion() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("competing-update", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (synced, _) =
            sync_workspace("competing-update", sync_options(source.path())).expect("sync");
        let lease = synced.prepared_workspace_lease.expect("lease");
        fs::write(source.path().join("file.txt"), "after\n").expect("update source");
        let barrier = Arc::new(Barrier::new(2));
        let path = source.path().display().to_string();

        std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_lease = lease.clone();
            let first_path = path.clone();
            let first = scope.spawn(move || {
                first_barrier.wait();
                update_workspace(
                    "competing-update",
                    RunnerWorkspaceUpdateOptions {
                        path: first_path,
                        lease: first_lease,
                    },
                )
            });
            let second_barrier = barrier.clone();
            let second = scope.spawn(move || {
                second_barrier.wait();
                update_workspace(
                    "competing-update",
                    RunnerWorkspaceUpdateOptions { path, lease },
                )
            });
            let successes = [
                first.join().expect("first thread"),
                second.join().expect("second thread"),
            ]
            .into_iter()
            .filter(Result::is_ok)
            .count();
            assert_eq!(successes, 1, "one lease holder must win the promotion");
        });
    });
}

#[test]
fn locked_prepared_workspace_rejects_update_without_mutating_live_state() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("locked-update", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (synced, _) =
            sync_workspace("locked-update", sync_options(source.path())).expect("sync");
        fs::write(source.path().join("file.txt"), "after\n").expect("update source");
        fs::create_dir(format!("{}.update-lock", synced.remote_path)).expect("hold update lock");

        update_workspace(
            "locked-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: synced.prepared_workspace_lease.expect("lease"),
            },
        )
        .expect_err("active update lock must reject competing promotion");

        assert_eq!(
            fs::read_to_string(Path::new(&synced.remote_path).join("file.txt")).unwrap(),
            "before\n"
        );
        assert!(Path::new(&synced.remote_path)
            .join(".homeboy/runner-workspace.json")
            .exists());
    });
}

#[test]
fn prepared_workspace_metadata_hydrates_execution_source_snapshot() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root");
        create_local_runner("execution-provenance", runner_root.path());
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (synced, _) =
            sync_workspace("execution-provenance", sync_options(source.path())).expect("sync");
        let original = synced.snapshot_identity.clone();
        fs::write(source.path().join("file.txt"), "after\n").expect("update source");
        let (updated, _) = update_workspace(
            "execution-provenance",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: synced.prepared_workspace_lease.expect("lease"),
            },
        )
        .expect("update");

        let mut source_snapshot = homeboy_core::source_snapshot::existing_remote(
            "execution-provenance",
            &updated.remote_path,
            Some(&runner_root.path().display().to_string()),
        );
        let runner = crate::load("execution-provenance").expect("runner");
        crate::hydrate_prepared_workspace_source_snapshot(
            &runner,
            &updated.remote_path,
            &mut source_snapshot,
        )
        .expect("hydrate execution source snapshot");
        assert_eq!(
            source_snapshot
                .prepared_workspace_original_snapshot_identity
                .as_deref(),
            Some(original.as_str())
        );
        assert_eq!(
            source_snapshot.prepared_workspace_update_lineage,
            updated.update_lineage
        );
    });
}

#[test]
fn prepared_workspace_update_quotes_runner_workspace_paths() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let parent = tempfile::tempdir().expect("runner parent");
        let marker = parent.path().join("injected");
        let runner_root = parent.path().join("runner root; touch injected");
        fs::create_dir_all(&runner_root).expect("runner root");
        create_local_runner("quoted-update", &runner_root);
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("file.txt"), "before\n").expect("source");
        let (synced, _) =
            sync_workspace("quoted-update", sync_options(source.path())).expect("sync");
        fs::write(source.path().join("file.txt"), "after\n").expect("update source");

        update_workspace(
            "quoted-update",
            RunnerWorkspaceUpdateOptions {
                path: source.path().display().to_string(),
                lease: synced.prepared_workspace_lease.expect("lease"),
            },
        )
        .expect("update with shell metacharacters in workspace root");

        assert_eq!(
            fs::read_to_string(Path::new(&synced.remote_path).join("file.txt")).unwrap(),
            "after\n"
        );
        assert!(
            !marker.exists(),
            "workspace path must not execute shell syntax"
        );
    });
}

fn create_local_runner(id: &str, workspace_root: &Path) {
    crate::create(
        &format!(
            r#"{{"id":"{id}","kind":"local","workspace_root":"{}"}}"#,
            workspace_root.display()
        ),
        false,
    )
    .expect("create runner");
}

fn create_ssh_runner(id: &str, workspace_root: &Path) {
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

fn sync_options(path: &Path) -> RunnerWorkspaceSyncOptions {
    RunnerWorkspaceSyncOptions {
        path: path.display().to_string(),
        mode: RunnerWorkspaceSyncMode::Snapshot,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        run_isolation_token: None,
    }
}
