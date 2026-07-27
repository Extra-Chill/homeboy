use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::workspace::sync::{
    active_resource_lifecycle_liveness, prune_scan_command, prune_workspaces,
    ssh_process_liveness_command, ssh_prune_delete_command,
    ssh_prune_delete_command_with_terminal_owner, sync_workspace,
    update_workspace_resource_lifecycle, ActiveResourceLifecycleLiveness, RunAuthority,
    WORKSPACE_METADATA_FILE,
};
use crate::workspace::types::{
    RunnerWorkspacePruneOptions, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::{MaterializedWorkspace, WorkspaceCleanupPolicy, WorkspaceTerminalOutcome};

#[test]
fn prune_workspaces_previews_orphans_without_deleting_by_default() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("orphan-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "hello\n").expect("source file");
        crate::create(
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
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1);
        assert_eq!(output.total_candidate_count, 1);
        assert!(output.total_candidate_bytes > 0);
        assert_eq!(output.remaining_candidate_count, 0);
        assert_eq!(output.remaining_candidate_bytes, 0);
        assert!(!output.has_more);
        assert!(output.next_command.is_none());
        assert!(output.drain_command.contains("--apply --min-age-hours 0"));
        assert_eq!(output.candidates[0].remote_path, synced.remote_path);
        assert_eq!(output.candidates[0].reason, "source_path_missing");
        assert!(Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_apply_removes_only_metadata_backed_orphans() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let orphan_source = source_parent.path().join("orphan-source");
        let live_source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&orphan_source).expect("orphan source dir");
        fs::create_dir_all(&live_source).expect("live source dir");
        fs::write(orphan_source.join("file.txt"), "orphan\n").expect("orphan file");
        fs::write(live_source.join("file.txt"), "live\n").expect("live file");
        crate::create(
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
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune apply");

        assert_eq!(exit_code, 0);
        assert!(!output.dry_run);
        assert_eq!(output.removed.len(), 1);
        assert_eq!(output.total_candidate_count, 1);
        assert!(output.total_candidate_bytes >= output.total_removed_bytes);
        assert_eq!(output.remaining_candidate_count, 0);
        assert!(!output.has_more);
        assert_eq!(output.removed[0].remote_path, orphan.remote_path);
        assert!(!Path::new(&orphan.remote_path).exists());
        assert!(Path::new(&live.remote_path).exists());
        assert!(unmanaged.exists());
    });
}

#[cfg(target_os = "linux")]
#[test]
fn prune_preserves_process_owned_workspace_in_preview_and_apply() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root.path().join("_lab_workspaces/process-owned");
        write_orphan_workspace(&workspace);
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-process-owned","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .current_dir(&workspace)
            .spawn()
            .expect("hold workspace cwd");

        for apply in [false, true] {
            let (output, exit_code) = prune_workspaces(
                "lab-local-prune-process-owned",
                RunnerWorkspacePruneOptions {
                    apply,
                    min_age_hours: 0,
                    limit: 10,
                    passes: 1,
                    cursor: None,
                },
            )
            .expect("prune process-owned workspace");
            assert_eq!(exit_code, 0);
            assert!(output.candidates.is_empty());
            assert_eq!(output.skipped_live_count, 1);
            assert!(workspace.exists());
        }
        child.kill().expect("stop held process");
        child.wait().expect("reap held process");
    });
}

#[test]
fn prune_preserves_active_job_lifecycle_lease() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root.path().join("_lab_workspaces/active-lease");
        write_orphan_workspace(&workspace);
        let metadata_path = workspace.join(WORKSPACE_METADATA_FILE);
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                .expect("metadata json");
        metadata["job_id"] = serde_json::json!("active-job");
        metadata["resource_lifecycle"] = serde_json::json!({
            "owner": "runner.workspace",
            "run_id": "active-job",
            "runner_id": null,
            "path": workspace.display().to_string(),
            "root_bound": runner_root.path().join("_lab_workspaces").display().to_string(),
            "kind": "runner_workspace",
            "ttl": null,
            "cleanup_policy": "delete_on_success",
            "evidence_retention": "metadata",
            "cleanup_intent": "dry_run",
            "cleanup_command": null,
            "status": "active",
        });
        fs::write(&metadata_path, metadata.to_string()).expect("write metadata");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-active-lease","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-active-lease",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune active lease workspace");

        assert_eq!(exit_code, 0);
        assert!(output.removed.is_empty());
        assert_eq!(output.skipped_live_count, 1);
        assert!(workspace.exists());
    });
}

#[test]
fn active_lifecycle_lease_requires_unambiguous_terminal_run_authority() {
    let metadata = active_lifecycle_metadata("run-terminal", "run-terminal");
    assert!(matches!(
        active_resource_lifecycle_liveness(&metadata, |_| RunAuthority::Terminal),
        ActiveResourceLifecycleLiveness::Terminal(owner) if owner == "run-terminal"
    ));
    assert!(matches!(
        active_resource_lifecycle_liveness(&metadata, |_| RunAuthority::Active),
        ActiveResourceLifecycleLiveness::Live
    ));
    assert!(matches!(
        active_resource_lifecycle_liveness(&metadata, |_| RunAuthority::Unavailable),
        ActiveResourceLifecycleLiveness::Unknown("active_resource_lifecycle_authority_unavailable")
    ));

    let absent = serde_json::json!({ "resource_lifecycle": { "status": "active" } });
    assert!(matches!(
        active_resource_lifecycle_liveness(&absent, |_| RunAuthority::Terminal),
        ActiveResourceLifecycleLiveness::NotActive
    ));

    let job_owned = serde_json::json!({
        "job_id": "active-job",
        "resource_lifecycle": { "status": "active" }
    });
    assert!(matches!(
        active_resource_lifecycle_liveness(&job_owned, |_| RunAuthority::Terminal),
        ActiveResourceLifecycleLiveness::Live
    ));

    let ambiguous = active_lifecycle_metadata("run-terminal", "other-run");
    assert!(matches!(
        active_resource_lifecycle_liveness(&ambiguous, |_| RunAuthority::Terminal),
        ActiveResourceLifecycleLiveness::Unknown("active_resource_lifecycle_owner_ambiguous")
    ));
}

#[test]
fn prune_workspaces_reaps_ttl_expired_lifecycle_workspace_with_live_source() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "live\n").expect("source file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-ttl","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (synced, _) = sync_workspace(
            "lab-local-prune-ttl",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        let metadata_path = Path::new(&synced.remote_path).join(".homeboy/runner-workspace.json");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                .expect("metadata json");
        metadata["resource_lifecycle"]["cleanup_policy"] = serde_json::json!("delete_after_ttl");
        metadata["resource_lifecycle"]["ttl"] = serde_json::json!("2020-01-01T00:00:00Z");
        fs::write(&metadata_path, metadata.to_string()).expect("write metadata");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-ttl",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert_eq!(output.candidates.len(), 1);
        assert_eq!(output.candidates[0].remote_path, synced.remote_path);
        assert_eq!(output.candidates[0].reason, "resource_ttl_expired");
        assert!(Path::new(&synced.remote_path).exists());
        assert!(source.exists());
    });
}

#[test]
fn preserved_failure_lifecycle_is_registered_for_ttl_pruning() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "live\n").expect("source file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-retained","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (synced, _) = sync_workspace(
            "lab-local-prune-retained",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        let mut lifecycle = synced.resource_lifecycle;
        lifecycle.cleanup_policy =
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteAfterTtl;
        lifecycle.ttl = Some("2020-01-01T00:00:00Z".to_string());
        lifecycle.cleanup_command = Some(
            "homeboy runner workspace prune lab-local-prune-retained --apply --min-age-hours 0"
                .to_string(),
        );
        update_workspace_resource_lifecycle(
            "lab-local-prune-retained",
            &synced.remote_path,
            lifecycle,
        )
        .expect("register ttl lifecycle");
        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-prune-retained".to_string(),
                synced.remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveOnFailure,
            );
            handle.set_terminal_outcome(WorkspaceTerminalOutcome::Failure);
        }
        let metadata =
            fs::read_to_string(Path::new(&synced.remote_path).join(WORKSPACE_METADATA_FILE))
                .expect("terminal metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["terminal_evidence"]["final_outcome"], "failure");
        assert_eq!(
            metadata["terminal_evidence"]["retained_location"],
            synced.remote_path
        );

        let (preview, _) = prune_workspaces(
            "lab-local-prune-retained",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune preview");

        assert_eq!(preview.candidates[0].reason, "resource_ttl_expired");
        assert_eq!(preview.candidates[0].remote_path, synced.remote_path);
        assert!(Path::new(&synced.remote_path).exists());

        let (applied, exit_code) = prune_workspaces(
            "lab-local-prune-retained",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
            },
        )
        .expect("apply ttl prune");

        assert_eq!(exit_code, 0);
        assert_eq!(applied.removed.len(), 1);
        assert_eq!(applied.removed[0].reason, "resource_ttl_expired");
        assert_eq!(applied.removed[0].remote_path, synced.remote_path);
        assert!(
            !Path::new(&synced.remote_path).exists(),
            "registered TTL lifecycle must be reaped by the owning runner prune path"
        );
    });
}

#[test]
fn uncertain_handoff_disarms_ttl_pruning() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "live\n").expect("source file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-handoff","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (synced, _) = sync_workspace(
            "lab-local-prune-handoff",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        let mut lifecycle = synced.resource_lifecycle;
        lifecycle.cleanup_policy =
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteAfterTtl;
        lifecycle.ttl = Some("2020-01-01T00:00:00Z".to_string());
        update_workspace_resource_lifecycle(
            "lab-local-prune-handoff",
            &synced.remote_path,
            lifecycle,
        )
        .expect("register ttl lifecycle");
        {
            let mut handle = MaterializedWorkspace::new(
                "lab-local-prune-handoff".to_string(),
                synced.remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::PreserveOnFailure,
            );
            handle.preserve();
        }

        let (preview, _) = prune_workspaces(
            "lab-local-prune-handoff",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune preview");

        assert!(preview.candidates.is_empty());
        assert!(Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_preview_reports_synthetic_odd_path_without_deleting() {
    homeboy_core::test_support::with_isolated_home(|_| {
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
        crate::create(
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
                passes: 1,
                cursor: None,
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
fn prune_workspaces_reports_remaining_bytes_and_drain_command_when_limited() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let source_a = source_parent.path().join("orphan-source-a");
        let source_b = source_parent.path().join("orphan-source-b");
        fs::create_dir_all(&source_a).expect("source a dir");
        fs::create_dir_all(&source_b).expect("source b dir");
        fs::write(source_a.join("file.txt"), "a\n").expect("source a file");
        fs::write(source_b.join("file.txt"), "larger b\n").expect("source b file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-limited","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        sync_workspace(
            "lab-local-prune-limited",
            sync_options(source_a.display().to_string()),
        )
        .expect("sync source a");
        sync_workspace(
            "lab-local-prune-limited",
            sync_options(source_b.display().to_string()),
        )
        .expect("sync source b");
        fs::remove_dir_all(&source_a).expect("remove source a");
        fs::remove_dir_all(&source_b).expect("remove source b");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-limited",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 1,
                passes: 1,
                cursor: None,
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1);
        assert_eq!(output.scanned_workspace_count, 1);
        assert!(!output.scan_complete);
        assert_eq!(output.total_candidate_count, 1);
        assert_eq!(output.remaining_candidate_count, 0);
        assert_eq!(output.remaining_candidate_bytes, 0);
        assert!(output.has_more);
        let cursor = output
            .continuation_cursor
            .as_deref()
            .expect("continuation cursor");
        assert!(output
            .next_command
            .as_deref()
            .is_some_and(|command| command.contains(&format!("--cursor {cursor}"))));
        assert!(output.drain_command.contains(&format!("--cursor {cursor}")));
    });
}

#[test]
fn prune_workspaces_apply_passes_drain_until_empty() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let source_a = source_parent.path().join("drain-source-a");
        let source_b = source_parent.path().join("drain-source-b");
        fs::create_dir_all(&source_a).expect("source a dir");
        fs::create_dir_all(&source_b).expect("source b dir");
        fs::write(source_a.join("file.txt"), "a\n").expect("source a file");
        fs::write(source_b.join("file.txt"), "b\n").expect("source b file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-drain","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (workspace_a, _) = sync_workspace(
            "lab-local-prune-drain",
            sync_options(source_a.display().to_string()),
        )
        .expect("sync source a");
        let (workspace_b, _) = sync_workspace(
            "lab-local-prune-drain",
            sync_options(source_b.display().to_string()),
        )
        .expect("sync source b");
        fs::remove_dir_all(&source_a).expect("remove source a");
        fs::remove_dir_all(&source_b).expect("remove source b");

        let (output, exit_code) = prune_workspaces(
            "lab-local-prune-drain",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 1,
                passes: 10,
                cursor: None,
            },
        )
        .expect("prune drain");

        assert_eq!(exit_code, 0);
        assert!(!output.dry_run);
        assert_eq!(output.scanned_workspace_count, 2);
        assert_eq!(output.total_candidate_count, 1);
        assert_eq!(output.removed.len(), 2);
        assert_eq!(output.remaining_candidate_count, 0);
        assert_eq!(output.remaining_candidate_bytes, 0);
        assert!(!output.has_more);
        assert!(output.next_command.is_none());
        assert!(!Path::new(&workspace_a.remote_path).exists());
        assert!(!Path::new(&workspace_b.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_advances_through_thousands_of_mixed_entries() {
    homeboy_core::test_support::with_isolated_home(|_| {
        const WORKSPACE_COUNT: usize = 5_214;
        const ORPHAN_INDICES: [usize; 3] = [1_333, 2_607, 5_213];
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspaces_root = runner_root.path().join("_lab_workspaces");
        fs::create_dir_all(&workspaces_root).expect("workspaces root");
        for index in 0..WORKSPACE_COUNT {
            let workspace = workspaces_root.join(format!("workspace-{index:05}"));
            if ORPHAN_INDICES.contains(&index) {
                write_orphan_workspace(&workspace);
            } else if index % 11 == 0 {
                fs::create_dir_all(workspace.join(".homeboy")).expect("malformed workspace");
                fs::write(workspace.join(WORKSPACE_METADATA_FILE), "{malformed")
                    .expect("malformed metadata");
            } else {
                fs::create_dir_all(workspace).expect("metadata-free workspace");
            }
        }
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-thousands","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let mut cursor = None;
        let mut scanned = 0;
        let mut removed = Vec::new();
        for _ in 0..20 {
            let (output, _) = prune_workspaces(
                "lab-local-prune-thousands",
                RunnerWorkspacePruneOptions {
                    apply: true,
                    min_age_hours: 0,
                    limit: 127,
                    passes: 3,
                    cursor,
                },
            )
            .expect("bounded mixed-entry drain");
            assert!(output.scanned_workspace_count <= 127 * 3);
            scanned += output.scanned_workspace_count;
            removed.extend(output.removed);
            if output.scan_complete {
                assert!(output.continuation_cursor.is_none());
                break;
            }
            cursor = Some(
                output
                    .continuation_cursor
                    .expect("partial scan continuation cursor"),
            );
        }

        assert_eq!(scanned, WORKSPACE_COUNT);
        assert_eq!(removed.len(), ORPHAN_INDICES.len());
        for index in ORPHAN_INDICES {
            assert!(!workspaces_root
                .join(format!("workspace-{index:05}"))
                .exists());
        }
        assert_eq!(
            fs::read_dir(&workspaces_root)
                .expect("remaining workspaces")
                .count(),
            WORKSPACE_COUNT - ORPHAN_INDICES.len()
        );
    });
}

#[test]
fn ssh_prune_scan_command_bounds_thousands_of_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    for index in 0..5_214 {
        write_orphan_workspace(&temp.path().join(format!("workspace-{index:05}")));
    }

    let output = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(
            &temp.path().display().to_string(),
            0,
            5,
            None,
        ))
        .output()
        .expect("run generated prune scan command");

    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .lines()
            .filter(|line| !line.starts_with("__homeboy_prune_scan__"))
            .count(),
        5,
        "{stdout}"
    );
    assert!(
        stdout.contains("__homeboy_prune_scan__\t5\tpartial"),
        "{stdout}"
    );

    let after = temp.path().join("workspace-00004");
    let resumed = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(
            &temp.path().display().to_string(),
            0,
            5,
            Some(&after),
        ))
        .output()
        .expect("resume generated prune scan command");
    assert!(resumed.status.success(), "{resumed:?}");
    let resumed_stdout = String::from_utf8_lossy(&resumed.stdout);
    assert!(!resumed_stdout.contains("workspace-00004\t"));
    assert!(resumed_stdout.contains("workspace-00005\t"));
    assert!(resumed_stdout.contains("workspace-00009\t"));
    assert!(
        resumed_stdout.contains("__homeboy_prune_scan__\t5\tpartial"),
        "{resumed_stdout}"
    );
}

#[test]
fn ssh_prune_scan_command_advances_past_a_timed_out_size_measurement() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_orphan_workspace(&temp.path().join("workspace-00000"));
    write_orphan_workspace(&temp.path().join("workspace-00001"));
    let commands = tempfile::tempdir().expect("fake command tempdir");
    let bin = commands.path().join("bin");
    fs::create_dir_all(&bin).expect("fake command dir");
    fs::write(bin.join("du"), "#!/bin/sh\n/bin/sleep 5\nexit 1\n").expect("fake du");
    Command::new("chmod")
        .args(["+x", &bin.join("du").display().to_string()])
        .status()
        .expect("make fake du executable");

    let started = Instant::now();
    let output = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(
            &temp.path().display().to_string(),
            0,
            1,
            None,
        ))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
        )
        .output()
        .expect("run generated prune scan command");

    assert!(output.status.success(), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\tunknown\n"), "{stdout}");
    assert!(
        stdout.contains("__homeboy_prune_scan__\t1\tpartial"),
        "{stdout}"
    );

    let after = temp.path().join("workspace-00000");
    let resumed = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(
            &temp.path().display().to_string(),
            0,
            1,
            Some(&after),
        ))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
        )
        .output()
        .expect("resume generated prune scan command");
    let resumed_stdout = String::from_utf8_lossy(&resumed.stdout);
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(
        resumed_stdout.contains("workspace-00001\t"),
        "{resumed_stdout}"
    );
    assert!(resumed_stdout.contains("\tunknown\n"), "{resumed_stdout}");
    assert!(resumed_stdout.contains("__homeboy_prune_scan__\t1\tcomplete\t\n"));
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
        .arg(prune_scan_command(&root.display().to_string(), 0, 10, None))
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
    assert!(stdout.lines().count() == 2, "{stdout}");
    assert!(
        stdout.contains("__homeboy_prune_scan__\t1\tcomplete"),
        "{stdout}"
    );
}

#[test]
fn ssh_shaped_liveness_probe_detects_process_cwd_ownership() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .current_dir(workspace.path())
        .spawn()
        .expect("hold workspace cwd");

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_process_liveness_command(
            &workspace.path().display().to_string(),
        ))
        .output()
        .expect("run SSH-shaped liveness probe");

    child.kill().expect("stop held process");
    child.wait().expect("reap held process");
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "live");
}

#[test]
fn ssh_shaped_prune_delete_revalidates_lifecycle_and_deletes_atomically() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("_lab_workspaces/orphan");
    write_orphan_workspace(&workspace);

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command(
            &root.path().join("_lab_workspaces").display().to_string(),
            &workspace.display().to_string(),
        ))
        .output()
        .expect("run atomic SSH-shaped prune delete");

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!workspace.exists());

    let active = root.path().join("_lab_workspaces/active");
    write_orphan_workspace(&active);
    let metadata_path = active.join(WORKSPACE_METADATA_FILE);
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
            .expect("metadata json");
    metadata["resource_lifecycle"] = serde_json::json!({ "status": "active" });
    fs::write(&metadata_path, metadata.to_string()).expect("write active metadata");

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command(
            &root.path().join("_lab_workspaces").display().to_string(),
            &active.display().to_string(),
        ))
        .output()
        .expect("run active lease SSH-shaped prune delete");

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "live:active_resource_lifecycle_lease"
    );
    assert!(active.exists());
}

#[test]
fn ssh_terminal_lifecycle_delete_revalidates_process_ownership() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("_lab_workspaces/terminal");
    write_orphan_workspace(&workspace);
    let metadata_path = workspace.join(WORKSPACE_METADATA_FILE);
    fs::write(
        &metadata_path,
        active_lifecycle_metadata("terminal-run", "terminal-run").to_string(),
    )
    .expect("write terminal lifecycle metadata");

    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .current_dir(&workspace)
        .spawn()
        .expect("hold workspace cwd after terminal authority");
    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_owner(
            &root.path().join("_lab_workspaces").display().to_string(),
            &workspace.display().to_string(),
            Some("terminal-run"),
        ))
        .output()
        .expect("revalidate process ownership before terminal lifecycle delete");

    child.kill().expect("stop held process");
    child.wait().expect("reap held process");
    assert!(output.status.success(), "{output:?}");
    assert_ne!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(workspace.exists());
}

#[test]
fn ssh_terminal_lifecycle_delete_requires_matching_owner_in_pretty_metadata() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspaces = root.path().join("_lab_workspaces");
    let terminal = workspaces.join("terminal");
    write_orphan_workspace(&terminal);
    fs::write(
        terminal.join(WORKSPACE_METADATA_FILE),
        serde_json::to_string_pretty(&active_lifecycle_metadata("terminal-run", "terminal-run"))
            .expect("pretty terminal metadata"),
    )
    .expect("write terminal lifecycle metadata");

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_owner(
            &workspaces.display().to_string(),
            &terminal.display().to_string(),
            Some("terminal-run"),
        ))
        .output()
        .expect("delete terminal-owned workspace");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!terminal.exists());

    let ambiguous = workspaces.join("ambiguous");
    write_orphan_workspace(&ambiguous);
    fs::write(
        ambiguous.join(WORKSPACE_METADATA_FILE),
        active_lifecycle_metadata("terminal-run", "other-run").to_string(),
    )
    .expect("write ambiguous lifecycle metadata");
    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_owner(
            &workspaces.display().to_string(),
            &ambiguous.display().to_string(),
            Some("terminal-run"),
        ))
        .output()
        .expect("retain ambiguous workspace");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "live:active_resource_lifecycle_lease"
    );
    assert!(ambiguous.exists());
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

fn write_orphan_workspace(path: &Path) {
    fs::create_dir_all(path.join(".homeboy")).expect("workspace metadata dir");
    fs::write(path.join("file.txt"), "orphan\n").expect("workspace file");
    fs::write(
        path.join(WORKSPACE_METADATA_FILE),
        serde_json::json!({
            "schema": "homeboy/runner-workspace/v1",
            "local_path": path.join("missing-source").display().to_string(),
        })
        .to_string(),
    )
    .expect("workspace metadata");
}

fn active_lifecycle_metadata(run_id: &str, resource_run_id: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/runner-workspace/v1",
        "run_id": run_id,
        "local_path": "/missing/source",
        "resource_lifecycle": {
            "run_id": resource_run_id,
            "status": "active"
        }
    })
}
