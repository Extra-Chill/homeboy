use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::workspace::sync::{
    active_resource_lifecycle_liveness, encoded_materialized_workspace_metadata_is_valid,
    has_terminal_delete_on_success_lifecycle_with, prune_scan_command, prune_size_command,
    prune_workspaces, revalidated_candidate_is_deletable, runner_job_liveness_with,
    ssh_process_liveness_command, ssh_prune_delete_command_with_terminal_authority,
    ssh_prune_delete_materialized_workspace_command, ssh_prune_delete_orphan_artifact_dir_command,
    sync_workspace, update_workspace_resource_lifecycle, workspace_liveness_with_size_observation,
    workspace_metadata_file, ActiveResourceLifecycleLiveness, RunAuthority,
};
use crate::workspace::types::{
    RunnerWorkspacePruneOptions, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::{MaterializedWorkspace, WorkspaceCleanupPolicy, WorkspaceTerminalOutcome};
use homeboy_core::api_jobs::JobStatus;

/// Why the scan produced the candidate set it did.
///
/// Every prune failure reported from CI has been a bare `left: 0, right: 1`
/// with no indication of which guard withheld the workspace, and #12715 stayed
/// unexplained for weeks behind exactly that comparison. The reasons were never
/// missing -- `prune_workspaces` already returns `skipped`,
/// `withheld_by_liveness_reason`, `scanned_workspace_count` and `scan_complete`
/// -- the assertions simply threw them away, so a failure that reproduces only
/// on CI could not be read from CI.
///
/// Same defect as the promotion provider assertion in #12741, which printed
/// `error.message` and discarded the `error.details` naming the operation.
fn prune_diagnosis(output: &crate::workspace::types::RunnerWorkspacePruneOutput) -> String {
    format!(
        "scanned={} scan_complete={} candidates={} removed={} skipped_live={} skipped_unknown={}\n  withheld_by_liveness_reason={:?}\n  skipped={:?}\n  workspace_root={} lab_workspaces_root={}",
        output.scanned_workspace_count,
        output.scan_complete,
        output.candidates.len(),
        output.removed.len(),
        output.skipped_live_count,
        output.skipped_unknown_count,
        output.withheld_by_liveness_reason,
        output.skipped,
        output.workspace_root,
        output.lab_workspaces_root,
    )
}

/// Seed a local runner definition into an explicitly injected config root, so
/// the test never touches the process-global env lock (#14362).
fn rooted_local_runner(roots: &homeboy_core::paths::PathRoots, id: &str, workspace_root: &Path) {
    use homeboy_core::config::ConfigEntity;
    fs::create_dir_all(<crate::Runner as ConfigEntity>::config_dir_in_root(
        roots.config(),
    ))
    .expect("runner config dir");
    let runner: crate::Runner = serde_json::from_str(&format!(
        r#"{{"id":"{id}","kind":"local","workspace_root":"{}"}}"#,
        workspace_root.display()
    ))
    .expect("runner spec");
    fs::write(
        <crate::Runner as ConfigEntity>::config_path_in_root(roots.config(), id),
        serde_json::to_string_pretty(&runner).expect("serialize runner"),
    )
    .expect("write runner config");
}

#[test]
fn prune_workspaces_previews_orphans_without_deleting_by_default() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("orphan-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "hello\n").expect("source file");
        rooted_local_runner(&roots, "lab-local-prune-preview", runner_root.path());
        let (synced, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-preview",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        fs::remove_dir_all(&source).expect("remove source");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-preview",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1, "{}", prune_diagnosis(&output));
        assert_eq!(output.total_candidate_count, 1);
        assert!(output.total_candidate_bytes > 0);
        assert_eq!(output.remaining_candidate_count, 0);
        assert_eq!(output.remaining_candidate_bytes, 0);
        assert!(!output.has_more);
        assert!(output.next_command.is_none());
        assert!(output.drain_command.contains("--apply --min-age-hours 0"));
        assert_eq!(output.candidates[0].remote_path, synced.remote_path);
        assert_eq!(
            output.candidates[0].reason,
            "stale_materialized_workspace_lifecycle"
        );
        assert!(Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_apply_removes_only_metadata_backed_orphans() {
    crate::test_support::with_isolated_home(|_| {
        {
            let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
            let source_parent = tempfile::tempdir().expect("source parent");
            let orphan_source = source_parent.path().join("orphan-source");
            let live_source = source_parent.path().join("live-source");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&orphan_source).expect("orphan source dir");
            fs::create_dir_all(&live_source).expect("live source dir");
            fs::write(orphan_source.join("file.txt"), "orphan\n").expect("orphan file");
            fs::write(live_source.join("file.txt"), "live\n").expect("live file");
            rooted_local_runner(&roots, "lab-local-prune-apply", runner_root.path());
            let (orphan, _) = crate::workspace::sync::sync_workspace(
                "lab-local-prune-apply",
                sync_options(orphan_source.display().to_string()),
            )
            .expect("sync orphan workspace");
            let (live, _) = crate::workspace::sync::sync_workspace(
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

            let (output, exit_code) = crate::workspace::sync::prune_workspaces(
                "lab-local-prune-apply",
                RunnerWorkspacePruneOptions {
                    apply: true,
                    min_age_hours: 0,
                    limit: 10,
                    passes: 1,
                    cursor: None,
                    ..RunnerWorkspacePruneOptions::default()
                },
            )
            .expect("prune apply");

            assert_eq!(exit_code, 0);
            assert!(!output.dry_run);
            assert_eq!(output.removed.len(), 2, "{}", prune_diagnosis(&output));
            assert_eq!(output.total_candidate_count, 2);
            assert!(output.total_candidate_bytes >= output.total_removed_bytes);
            assert_eq!(output.remaining_candidate_count, 0);
            // The metadata-free directory is reported, not deleted, and does not
            // keep the drain open: rerunning cannot prove its ownership.
            assert!(!output.has_more);
            assert_eq!(output.skipped_unknown_count, 1);
            assert_eq!(
                output.skipped[0].remote_path,
                unmanaged.display().to_string()
            );
            assert!(output.skipped[0]
                .reason
                .contains("workspace_metadata_missing"));
            assert!(output
                .removed
                .iter()
                .any(|entry| entry.remote_path == orphan.remote_path));
            assert!(!Path::new(&orphan.remote_path).exists());
            assert!(!Path::new(&live.remote_path).exists());
            assert!(unmanaged.exists());
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn prune_preserves_process_owned_workspace_in_preview_and_apply() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root.path().join("_lab_workspaces/process-owned");
        write_orphan_workspace(&workspace);
        rooted_local_runner(&roots, "lab-local-prune-process-owned", runner_root.path());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .current_dir(&workspace)
            .spawn()
            .expect("hold workspace cwd");

        for apply in [false, true] {
            let (output, exit_code) = crate::workspace::sync::prune_workspaces(
                "lab-local-prune-process-owned",
                RunnerWorkspacePruneOptions {
                    apply,
                    min_age_hours: 0,
                    limit: 10,
                    passes: 1,
                    cursor: None,
                    ..RunnerWorkspacePruneOptions::default()
                },
            )
            .expect("prune process-owned workspace");
            assert_eq!(exit_code, 0);
            assert!(output.candidates.is_empty());
            assert_eq!(output.skipped_live_count, 1, "{output:#?}");
            assert!(workspace.exists());
        }
        child.kill().expect("stop held process");
        child.wait().expect("reap held process");
    });
}

#[test]
fn prune_preserves_job_lifecycle_lease_when_authority_is_unavailable() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root.path().join("_lab_workspaces/active-lease");
        write_orphan_workspace(&workspace);
        let metadata_path = workspace_metadata_file(&workspace);
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                .expect("metadata json");
        metadata["run_id"] = serde_json::json!("active-job");
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
        rooted_local_runner(&roots, "lab-local-prune-active-lease", runner_root.path());

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-active-lease",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune active lease workspace");

        assert_eq!(exit_code, 0);
        assert!(output.removed.is_empty());
        assert_eq!(output.skipped_unknown_count, 1);
        assert_eq!(output.withheld_by_liveness_reason.len(), 1);
        assert_eq!(
            output.withheld_by_liveness_reason[0].reason,
            "active_resource_lifecycle_authority_unavailable"
        );
        assert_eq!(output.withheld_by_liveness_reason[0].workspace_count, 1);
        assert!(output.withheld_by_liveness_reason[0].bytes > 0);
        assert!(workspace.exists());
    });
}

#[test]
fn retained_terminal_receipt_reclassifies_and_releases_compacted_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let runner_id = "lab-local-prune-terminal-receipt";
        let run_id = "compacted-terminal-run";
        let job_id = "job-from-prior-runner-generation";
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("retained-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root.path().join("_lab_workspaces/retained-terminal");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "source\n").expect("source file");
        write_terminal_workspace(&workspace, &source, runner_root.path(), run_id);
        crate::create(
            &format!(
                r#"{{"id":"{runner_id}","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        homeboy_agents::agent_task_lifecycle::persist_workspace_terminal_authority_for_test(
            run_id,
            runner_id,
            job_id,
            &workspace.display().to_string(),
        )
        .expect("persist retained terminal authority");

        let (preview, _) = prune_workspaces(
            runner_id,
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("preview receipt-authorized workspace");
        assert_eq!(preview.candidates.len(), 1, "{}", prune_diagnosis(&preview));
        assert_eq!(preview.candidates[0].reason, "terminal_resource_lifecycle");
        assert!(preview.withheld_by_liveness_reason.is_empty());
        homeboy_agents::agent_task_lifecycle::begin_workspace_terminal_authority_release(
            run_id,
            runner_id,
            &workspace.display().to_string(),
        )
        .expect("simulate interruption before workspace deletion");

        let (applied, _) = prune_workspaces(
            runner_id,
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("remove receipt-authorized workspace");
        assert_eq!(applied.removed.len(), 1, "{}", prune_diagnosis(&applied));
        assert!(!workspace.exists());
        homeboy_agents::agent_task_lifecycle::persist_workspace_terminal_authority_for_test(
            run_id,
            runner_id,
            job_id,
            &workspace.display().to_string(),
        )
        .expect("late terminal projection is suppressed after deletion");
        assert!(
            homeboy_agents::agent_task_lifecycle::resolve_workspace_terminal_authority(
                run_id,
                runner_id,
                &workspace.display().to_string(),
                Some(job_id),
            )
            .is_err()
        );
    });
}

/// A Lab gate owns its declared-resource and candidate workspaces through its
/// generic runner-exec run (`lab-gate-*`), which lives in the observation
/// `runs` table rather than the agent-task lifecycle store. Prune must resolve
/// that run kind: terminal releases the lease, running keeps it live, and an
/// unknown owner stays fail-closed.
#[test]
fn declared_gate_workspace_resolves_its_runner_exec_run_authority() {
    crate::test_support::with_isolated_home(|_| {
        let runner_id = "lab-local-prune-gate-run";
        let source_parent = tempfile::tempdir().expect("source parent");
        // The controller's private resource package is gone after the gate, as
        // on the Lab: every workspace is an orphan candidate whose deletion
        // depends only on its owning run's authority.
        let source = source_parent.path().join("declared-gate-resources");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        crate::create(
            &format!(
                r#"{{"id":"{runner_id}","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let lab = runner_root.path().join("_lab_workspaces");
        let terminal = lab.join("declared-gate-resources-terminal");
        let running = lab.join("declared-gate-resources-running");
        let unknown = lab.join("declared-gate-resources-unknown");
        let other_runner = lab.join("declared-gate-resources-other-runner");
        write_terminal_workspace(&terminal, &source, runner_root.path(), "lab-gate-terminal");
        write_terminal_workspace(&running, &source, runner_root.path(), "lab-gate-running");
        write_terminal_workspace(&unknown, &source, runner_root.path(), "lab-gate-unknown");
        write_terminal_workspace(
            &other_runner,
            &source,
            runner_root.path(),
            "lab-gate-other-runner",
        );
        let command = ["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, owner) in [
            ("lab-gate-terminal", runner_id),
            ("lab-gate-running", runner_id),
            ("lab-gate-other-runner", "another-runner"),
        ] {
            homeboy_agents::agent_task_lifecycle::ensure_generic_runner_exec_run(
                run_id,
                owner,
                &source.display().to_string(),
                &command,
            )
            .expect("gate run");
        }
        for run_id in ["lab-gate-terminal", "lab-gate-other-runner"] {
            assert!(
                homeboy_agents::agent_task_lifecycle::finish_runner_exec_pre_handoff_failure(
                    run_id,
                    "lab_gate",
                    "transport",
                    false,
                    &homeboy_core::Error::internal_unexpected("gate finished"),
                )
                .expect("terminalize gate run")
            );
        }

        let (preview, _) = prune_workspaces(
            runner_id,
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("preview gate workspaces");

        let candidates = preview
            .candidates
            .iter()
            .map(|candidate| (candidate.remote_path.clone(), candidate.reason.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            candidates,
            vec![(
                terminal.display().to_string(),
                "terminal_resource_lifecycle".to_string()
            )],
            "{}",
            prune_diagnosis(&preview)
        );
        assert_eq!(
            preview.skipped_live_count,
            1,
            "{}",
            prune_diagnosis(&preview)
        );
        assert_eq!(
            preview.skipped_unknown_count,
            2,
            "{}",
            prune_diagnosis(&preview)
        );
        assert!(running.exists() && unknown.exists() && other_runner.exists());
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
        ActiveResourceLifecycleLiveness::NotActive
    ));

    let ambiguous = active_lifecycle_metadata("run-terminal", "other-run");
    assert!(matches!(
        active_resource_lifecycle_liveness(&ambiguous, |_| RunAuthority::Terminal),
        ActiveResourceLifecycleLiveness::Unknown("active_resource_lifecycle_owner_ambiguous")
    ));
}

#[test]
fn terminal_delete_on_success_lifecycle_requires_exact_durable_authority() {
    let mut metadata = active_lifecycle_metadata("run-terminal", "run-terminal");
    metadata["resource_lifecycle"]["cleanup_policy"] = serde_json::json!("delete_on_success");
    assert!(has_terminal_delete_on_success_lifecycle_with(
        &metadata,
        |_| RunAuthority::Terminal,
    ));
    assert!(!has_terminal_delete_on_success_lifecycle_with(
        &metadata,
        |_| RunAuthority::Active,
    ));
    assert!(!has_terminal_delete_on_success_lifecycle_with(
        &metadata,
        |_| RunAuthority::Unavailable,
    ));

    let mut ambiguous = active_lifecycle_metadata("run-terminal", "other-run");
    ambiguous["resource_lifecycle"]["cleanup_policy"] = serde_json::json!("delete_on_success");
    assert!(!has_terminal_delete_on_success_lifecycle_with(
        &ambiguous,
        |_| RunAuthority::Terminal,
    ));
}

#[test]
fn runner_job_authority_requires_exact_transport_state_and_consistent_snapshot() {
    let active = vec!["job-1".to_string()];
    for (name, exact, snapshot, state) in [
        (
            "direct-live",
            Ok(JobStatus::Running),
            Some(&active[..]),
            "live",
        ),
        (
            "queued-live",
            Ok(JobStatus::Queued),
            Some(&active[..]),
            "live",
        ),
        (
            "terminal",
            Ok(JobStatus::Succeeded),
            Some(&[][..]),
            "inactive",
        ),
        ("absent", Err(absent_job_error()), Some(&[][..]), "inactive"),
        (
            "unavailable",
            Err(homeboy_core::Error::internal_unexpected(
                "transport unavailable",
            )),
            None,
            "unknown",
        ),
        (
            "terminal-listed",
            Ok(JobStatus::Failed),
            Some(&active[..]),
            "unknown",
        ),
        (
            "live-omitted",
            Ok(JobStatus::Running),
            Some(&[][..]),
            "unknown",
        ),
        (
            "absent-listed",
            Err(absent_job_error()),
            Some(&active[..]),
            "unknown",
        ),
    ] {
        assert_eq!(
            runner_job_liveness_with("job-1", exact, snapshot).state,
            state,
            "{name}"
        );
    }
}

#[test]
fn non_agent_task_polling_loss_retains_job_owned_workspace_until_terminal_reconciliation() {
    crate::test_support::with_isolated_home(|_| {
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("review-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "review\n").expect("source file");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-review-poll-loss","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (synced, _) = sync_workspace(
            "lab-local-review-poll-loss",
            sync_options(source.display().to_string()),
        )
        .expect("sync review workspace");

        // A review has no agent-task run id, but a live accepted daemon job
        // still owns its workspace after the controller loses polling.
        {
            let mut workspace = MaterializedWorkspace::new(
                "lab-local-review-poll-loss".to_string(),
                synced.remote_path.clone(),
                None,
                WorkspaceCleanupPolicy::DeleteAlways,
            );
            workspace.retain_for_reconciliation("review-job-11380", Some("generation-review"));
        }

        let metadata = fs::read_to_string(workspace_metadata_file(&synced.remote_path))
            .expect("retained workspace metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["job_id"], "review-job-11380");
        assert_eq!(
            metadata["terminal_evidence"]["reconciliation"]["daemon_generation"],
            "generation-review"
        );

        let active = vec!["review-job-11380".to_string()];
        let polling_loss =
            runner_job_liveness_with("review-job-11380", Ok(JobStatus::Running), Some(&active));
        assert_eq!(polling_loss.state, "live");
        assert!(Path::new(&synced.remote_path).exists());

        let terminal =
            runner_job_liveness_with("review-job-11380", Ok(JobStatus::Succeeded), Some(&[]));
        assert_eq!(terminal.state, "inactive");
        assert!(
            revalidated_candidate_is_deletable(&terminal),
            "terminal reconciliation authorizes the retained workspace for cleanup"
        );
    });
}

#[test]
fn prune_revalidates_job_authority_before_deletion() {
    let scanned = runner_job_liveness_with("job-1", Ok(JobStatus::Succeeded), Some(&[]));
    let revalidated = runner_job_liveness_with(
        "job-1",
        Ok(JobStatus::Running),
        Some(&["job-1".to_string()]),
    );

    assert!(revalidated_candidate_is_deletable(&scanned));
    assert!(!revalidated_candidate_is_deletable(&revalidated));
}

fn absent_job_error() -> homeboy_core::Error {
    let mut error = homeboy_core::Error::internal_unexpected("daemon request returned HTTP 404");
    error.details["daemon_transport_error"] = serde_json::json!({ "http_status": 404 });
    error
}

#[test]
fn prune_workspaces_reaps_ttl_expired_lifecycle_workspace_with_live_source() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "live\n").expect("source file");
        rooted_local_runner(&roots, "lab-local-prune-ttl", runner_root.path());
        let (synced, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-ttl",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        let metadata_path = workspace_metadata_file(&synced.remote_path);
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                .expect("metadata json");
        metadata["resource_lifecycle"]["cleanup_policy"] = serde_json::json!("delete_after_ttl");
        metadata["resource_lifecycle"]["ttl"] = serde_json::json!("2020-01-01T00:00:00Z");
        fs::write(&metadata_path, metadata.to_string()).expect("write metadata");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-ttl",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert_eq!(output.candidates.len(), 1, "{}", prune_diagnosis(&output));
        assert_eq!(output.candidates[0].remote_path, synced.remote_path);
        assert_eq!(output.candidates[0].reason, "resource_ttl_expired");
        assert!(Path::new(&synced.remote_path).exists());
        assert!(source.exists());
    });
}

#[test]
fn prune_workspaces_reaps_stale_materialized_workspace_with_live_source() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("live-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "live\n").expect("source file");
        rooted_local_runner(&roots, "lab-local-prune-materialized", runner_root.path());
        let (synced, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-materialized",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-materialized",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune stale materialized workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.removed.len(), 1, "{}", prune_diagnosis(&output));
        assert_eq!(
            output.removed[0].reason,
            "stale_materialized_workspace_lifecycle"
        );
        assert!(!Path::new(&synced.remote_path).exists());
        assert!(source.exists());
    });
}

#[test]
fn prune_workspaces_prefers_stale_materialized_lifecycle_when_source_is_missing() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let source = source_parent.path().join("removed-source");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("file.txt"), "removed\n").expect("source file");
        rooted_local_runner(
            &roots,
            "lab-local-prune-materialized-missing",
            runner_root.path(),
        );
        let (synced, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-materialized-missing",
            sync_options(source.display().to_string()),
        )
        .expect("sync workspace");
        fs::remove_dir_all(&source).expect("remove source");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-materialized-missing",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune stale materialized workspace with missing source");

        assert_eq!(exit_code, 0);
        assert_eq!(output.removed.len(), 1, "{}", prune_diagnosis(&output));
        assert_eq!(
            output.removed[0].reason,
            "stale_materialized_workspace_lifecycle"
        );
        assert!(!Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn preserved_failure_lifecycle_is_registered_for_ttl_pruning() {
    crate::test_support::with_isolated_home(|_| {
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
        let metadata = fs::read_to_string(workspace_metadata_file(&synced.remote_path))
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
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert_eq!(preview.candidates.len(), 1, "{}", prune_diagnosis(&preview));
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
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("apply ttl prune");

        assert_eq!(exit_code, 0);
        assert_eq!(applied.removed.len(), 1, "{}", prune_diagnosis(&applied));
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
    crate::test_support::with_isolated_home(|_| {
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
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert!(preview.candidates.is_empty());
        assert!(Path::new(&synced.remote_path).exists());
    });
}

#[test]
fn prune_workspaces_preview_reports_synthetic_odd_path_without_deleting() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspace = runner_root
            .path()
            .join("_lab_workspaces")
            .join("repo's odd (name) with spaces");
        fs::create_dir_all(&workspace).expect("workspace metadata dir");
        fs::create_dir_all(workspace_metadata_file(&workspace).parent().unwrap())
            .expect("workspace metadata dir");
        fs::write(workspace.join("file.txt"), "orphan\n").expect("workspace file");
        fs::write(
            workspace_metadata_file(&workspace),
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
        rooted_local_runner(&roots, "lab-local-prune-odd-preview", runner_root.path());

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-odd-preview",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 10,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1, "{}", prune_diagnosis(&output));
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
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let source_a = source_parent.path().join("orphan-source-a");
        let source_b = source_parent.path().join("orphan-source-b");
        fs::create_dir_all(&source_a).expect("source a dir");
        fs::create_dir_all(&source_b).expect("source b dir");
        fs::write(source_a.join("file.txt"), "a\n").expect("source a file");
        fs::write(source_b.join("file.txt"), "larger b\n").expect("source b file");
        rooted_local_runner(&roots, "lab-local-prune-limited", runner_root.path());
        crate::workspace::sync::sync_workspace(
            "lab-local-prune-limited",
            sync_options(source_a.display().to_string()),
        )
        .expect("sync source a");
        crate::workspace::sync::sync_workspace(
            "lab-local-prune-limited",
            sync_options(source_b.display().to_string()),
        )
        .expect("sync source b");
        fs::remove_dir_all(&source_a).expect("remove source a");
        fs::remove_dir_all(&source_b).expect("remove source b");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-limited",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 0,
                limit: 1,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        assert_eq!(exit_code, 0);
        assert!(output.dry_run);
        assert_eq!(output.candidates.len(), 1, "{}", prune_diagnosis(&output));
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
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let source_parent = tempfile::tempdir().expect("source parent");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let source_a = source_parent.path().join("drain-source-a");
        let source_b = source_parent.path().join("drain-source-b");
        fs::create_dir_all(&source_a).expect("source a dir");
        fs::create_dir_all(&source_b).expect("source b dir");
        fs::write(source_a.join("file.txt"), "a\n").expect("source a file");
        fs::write(source_b.join("file.txt"), "b\n").expect("source b file");
        rooted_local_runner(&roots, "lab-local-prune-drain", runner_root.path());
        let (workspace_a, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-drain",
            sync_options(source_a.display().to_string()),
        )
        .expect("sync source a");
        let (workspace_b, _) = crate::workspace::sync::sync_workspace(
            "lab-local-prune-drain",
            sync_options(source_b.display().to_string()),
        )
        .expect("sync source b");
        fs::remove_dir_all(&source_a).expect("remove source a");
        fs::remove_dir_all(&source_b).expect("remove source b");

        let (output, exit_code) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-drain",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 1,
                passes: 10,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune drain");

        assert_eq!(exit_code, 0);
        assert!(!output.dry_run);
        assert_eq!(output.scanned_workspace_count, 2);
        assert_eq!(output.total_candidate_count, 1);
        assert_eq!(output.removed.len(), 2, "{}", prune_diagnosis(&output));
        assert_eq!(output.remaining_candidate_count, 0);
        assert_eq!(output.remaining_candidate_bytes, 0);
        assert!(!output.has_more);
        assert!(output.next_command.is_none());
        assert!(!Path::new(&workspace_a.remote_path).exists());
        assert!(!Path::new(&workspace_b.remote_path).exists());
    });
}

#[test]
fn prune_convergence_resumes_durable_receipts_across_more_than_twenty_pages() {
    crate::test_support::with_isolated_home(|_| {
        {
            let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
            const REMOVABLE_COUNT: usize = 23;
            const WORKSPACE_COUNT: usize = REMOVABLE_COUNT + 2;
            let runner_root = tempfile::tempdir().expect("runner root");
            let workspaces_root = runner_root.path().join("_lab_workspaces");
            for index in 0..REMOVABLE_COUNT {
                write_orphan_workspace(&workspaces_root.join(format!("removable-{index:02}")));
            }
            let active = workspaces_root.join("active-process");
            write_orphan_workspace(&active);
            let unknown = workspaces_root.join("unknown-authority");
            write_orphan_workspace(&unknown);
            let metadata_path = workspace_metadata_file(&unknown);
            let mut metadata: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
                    .expect("metadata JSON");
            metadata["run_id"] = serde_json::json!("authority-unavailable");
            metadata["resource_lifecycle"] = serde_json::json!({
                "owner": "runner.workspace",
                "run_id": "authority-unavailable",
                "runner_id": null,
                "path": unknown.display().to_string(),
                "root_bound": workspaces_root.display().to_string(),
                "kind": "runner_workspace",
                "ttl": null,
                "status": "active",
                "cleanup_policy": "delete_on_success",
                "evidence_retention": "metadata",
                "cleanup_intent": "dry_run",
                "cleanup_command": null,
            });
            fs::write(&metadata_path, metadata.to_string()).expect("write unknown metadata");
            rooted_local_runner(&roots, "lab-local-prune-convergence", runner_root.path());
            let mut child = Command::new("sh")
                .arg("-c")
                .arg("sleep 30")
                .current_dir(&active)
                .spawn()
                .expect("hold active workspace cwd");

            let (interrupted, _) = crate::workspace::sync::prune_workspaces(
                "lab-local-prune-convergence",
                RunnerWorkspacePruneOptions {
                    apply: true,
                    min_age_hours: 0,
                    limit: 1,
                    passes: 7,
                    converge: true,
                    ..RunnerWorkspacePruneOptions::default()
                },
            )
            .expect("bounded convergence");
            assert_eq!(interrupted.skipped_live_count, 1);
            let interrupted = interrupted.convergence.expect("convergence evidence");
            assert_eq!(interrupted.pass_count, 7);
            assert_eq!(interrupted.terminal_reason, "max_passes");
            assert!(interrupted.resume_command.is_some());
            assert!(Path::new(&interrupted.receipt_path).is_file());
            assert_eq!(interrupted.cursor_history.len(), interrupted.pass_count * 2);

            let (resumed, _) = crate::workspace::sync::prune_workspaces(
                "lab-local-prune-convergence",
                RunnerWorkspacePruneOptions {
                    apply: true,
                    min_age_hours: 0,
                    limit: 1,
                    passes: 30,
                    converge: true,
                    resume: true,
                    ..RunnerWorkspacePruneOptions::default()
                },
            )
            .expect("resume convergence");
            assert_eq!(resumed.skipped_unknown_count, 1);
            let resumed = resumed.convergence.expect("resumed convergence evidence");
            // `limit` bounds candidates, so withheld workspaces never end a pass:
            // one pass per removable workspace plus the pass that reaches the end.
            assert_eq!(resumed.pass_count, REMOVABLE_COUNT + 1);
            // Each retained workspace also carries its `-homeboy-state` sibling,
            // inventoried as owned by its checkout; removed workspaces take their
            // sibling with them before the scan reaches it.
            assert_eq!(resumed.inspected_count, WORKSPACE_COUNT + 2);
            assert_eq!(resumed.applied_count, REMOVABLE_COUNT);
            assert_eq!(resumed.skipped_count, 2);
            assert_eq!(resumed.terminal_reason, "scan_complete");
            assert!(resumed.resume_command.is_none());
            assert_eq!(resumed.cursor_history.len(), resumed.pass_count * 2);
            assert_eq!(
                resumed.verified_reclaimed_bytes,
                resumed
                    .page_receipts
                    .iter()
                    .map(|receipt| receipt.reclaimed_bytes)
                    .sum::<u64>()
            );
            assert!(resumed.verified_reclaimed_bytes > 0);
            assert!(active.exists());
            assert!(unknown.exists());
            assert_eq!(
                fs::read_dir(workspaces_root)
                    .expect("remaining workspaces")
                    .count(),
                4
            );
            child.kill().expect("stop active workspace holder");
            child.wait().expect("reap active workspace holder");
        }
    });
}

/// Age a Lab workspace entry past the prune age gate.
fn age_entry(path: &Path) {
    let status = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(path)
        .status()
        .expect("age workspace entry");
    assert!(status.success());
}

/// A metadata-backed workspace shaped like `sync_workspace` writes it.
fn write_family_workspace(path: &Path, local_path: &Path, run_id: Option<&str>) {
    fs::create_dir_all(&path).expect("workspace metadata dir");
    fs::create_dir_all(workspace_metadata_file(path).parent().unwrap())
        .expect("workspace metadata dir");
    fs::write(path.join("file.txt"), "workspace\n").expect("workspace file");
    let mut metadata = serde_json::json!({
        "schema": "homeboy/runner-workspace/v1",
        "runner_id": "lab-local-prune-families",
        "repo": path.file_name().and_then(|name| name.to_str()),
        "local_path": local_path.display().to_string(),
        "remote_path": path.display().to_string(),
        "sync_mode": "snapshot",
        "snapshot_identity": "snapshot:0123456789abcdef",
        "synced_at": "2026-09-29T00:00:00Z",
    });
    if let Some(run_id) = run_id {
        metadata["run_id"] = serde_json::json!(run_id);
        metadata["resource_lifecycle"] = serde_json::json!({
            "owner": "runner.workspace",
            "run_id": run_id,
            "runner_id": "lab-local-prune-families",
            "path": path.display().to_string(),
            "kind": "runner_workspace",
            "cleanup_policy": "delete_on_success",
            "evidence_retention": "metadata",
            "cleanup_intent": "dry_run",
            "status": "active"
        });
    }
    fs::write(workspace_metadata_file(path), metadata.to_string()).expect("metadata");
}

/// Live Lab: 1,515 top-level entries, yet a cursorless prune page (what the
/// automatic `remote_lab_workspaces` cleanup runs) only ever inspected the
/// first `limit` names, and entries without runner-workspace metadata (the
/// `<checkout>-homeboy-artifacts` siblings, manual checkouts) were counted but
/// never planned nor reported. Every top-level entry must be inventoried:
/// stale workspaces behind a retained prefix are planned, orphaned artifact
/// siblings are planned on their checkout ownership proof, and entries with
/// no ownership proof are reported as unknown with a reason.
#[test]
fn prune_page_inventories_every_lab_workspace_family_behind_a_retained_prefix() {
    crate::test_support::with_isolated_home(|_| {
        let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
        let runner_root = tempfile::tempdir().expect("runner root");
        let sources = tempfile::tempdir().expect("sources");
        let live_source = sources.path().join("live-source");
        fs::create_dir_all(&live_source).expect("live source");
        let missing_source = sources.path().join("removed-source");
        let root = runner_root.path().join("_lab_workspaces");
        fs::create_dir_all(&root).expect("lab workspaces root");

        // A retained prefix larger than the page size, sorted ahead of everything.
        for index in 0..30 {
            write_family_workspace(
                &root.join(format!("aaa-retained-{index:02}")),
                &live_source,
                None,
            );
        }
        // Stale orphans of the live name families.
        let agent = root
            .join("agent-0de3a73a49b8-9f98df59-c4d4-4101-a210-af5f018c6e0a-1792021408680403000");
        let baseline = root.join(
        "baseline-185d71f1-19bf-41c6-8abf-934993d7d5fc-46522c0b2765-28cf9b26-1791387195668401000",
    );
        let fix_issue =
            root.join("homeboy-fix-issue-14832-homeboy-75f172ad2571-0f62e000-1792000000000000000");
        for path in [&agent, &baseline, &fix_issue] {
            write_family_workspace(path, &missing_source, None);
        }
        // Run-owned lease whose run authority is unavailable: withheld, reported.
        let declared = root.join("declared-gate-resources-9f1ff7e143d7-1f241e11-91b7-47b9-b6ff-aa28b5f6e127-1792021117096117000");
        write_family_workspace(
            &declared,
            &missing_source,
            Some("run-authority-unavailable"),
        );
        // Artifact sibling whose checkout is gone, and one whose checkout lives.
        let orphan_artifacts = root.join("zz-gone-checkout-homeboy-artifacts");
        fs::create_dir_all(orphan_artifacts.join("provenance")).expect("orphan artifacts");
        fs::write(orphan_artifacts.join("provenance/receipt.json"), "{}").expect("receipt");
        let kept = root.join("zz-kept-checkout");
        write_family_workspace(&kept, &live_source, None);
        let kept_artifacts = root.join("zz-kept-checkout-homeboy-artifacts");
        fs::create_dir_all(kept_artifacts.join("provenance")).expect("kept artifacts");
        // No ownership proof at all.
        let manual = root.join("manual-checkout");
        fs::create_dir_all(manual.join(".git")).expect("manual checkout");
        let stray = root.join("stray-file");
        fs::write(&stray, "stray").expect("stray file");
        for entry in fs::read_dir(&root).expect("read root") {
            age_entry(&entry.expect("entry").path());
        }
        // Fresh orphan: below the age gate.
        let young = root.join("young-orphan");
        write_family_workspace(&young, &missing_source, None);
        let entry_count = fs::read_dir(&root).expect("read root").count();
        rooted_local_runner(&roots, "lab-local-prune-families", runner_root.path());

        let (output, _) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-families",
            RunnerWorkspacePruneOptions {
                apply: false,
                min_age_hours: 1,
                limit: 25,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune preview");

        let mut planned = output
            .candidates
            .iter()
            .map(|candidate| (candidate.remote_path.clone(), candidate.reason.clone()))
            .collect::<Vec<_>>();
        planned.sort();
        let path = |path: &Path| path.display().to_string();
        assert_eq!(
            planned,
            vec![
                (path(&agent), "source_path_missing".to_string()),
                (path(&baseline), "source_path_missing".to_string()),
                (path(&fix_issue), "source_path_missing".to_string()),
                (
                    path(&orphan_artifacts),
                    "artifact_dir_checkout_missing".to_string()
                ),
            ],
            "{}",
            prune_diagnosis(&output)
        );
        assert_eq!(
            output.scanned_workspace_count,
            entry_count,
            "{}",
            prune_diagnosis(&output)
        );
        assert!(output.scan_complete, "{}", prune_diagnosis(&output));
        let skipped_reason = |target: &Path| {
            output
                .skipped
                .iter()
                .find(|entry| entry.remote_path == path(target))
                .map(|entry| entry.reason.clone())
                .unwrap_or_default()
        };
        assert!(
            skipped_reason(&declared).contains("active_resource_lifecycle_authority_unavailable"),
            "{}",
            prune_diagnosis(&output)
        );
        assert!(
            skipped_reason(&manual).contains("workspace_metadata_missing"),
            "{}",
            prune_diagnosis(&output)
        );
        assert!(
            skipped_reason(&stray).contains("not_a_directory"),
            "{}",
            prune_diagnosis(&output)
        );
        assert_eq!(output.skipped.len(), 3, "{}", prune_diagnosis(&output));
        assert_eq!(output.skipped_unknown_count, 3);
        let retained = output
            .retained_by_reason
            .iter()
            .map(|retained| (retained.reason.as_str(), retained.workspace_count))
            .collect::<Vec<_>>();
        assert_eq!(
            retained,
            vec![
                ("artifact_dir_owned_by_checkout", 1),
                ("source_path_present", 31),
                // Every metadata-backed workspace's `-homeboy-state` sibling
                // (30 retained prefix + 3 stale + declared + kept) follows its
                // checkout; the young orphan's sibling is gated by age.
                ("state_dir_owned_by_checkout", 35),
                ("younger_than_min_age", 2),
            ]
        );
        // Every inspected entry is accounted for exactly once.
        assert_eq!(
            output.candidates.len()
                + output.skipped.len()
                + retained.iter().map(|(_, count)| count).sum::<usize>(),
            entry_count
        );
        assert!(young.exists() && orphan_artifacts.exists() && manual.exists());

        let (applied, _) = crate::workspace::sync::prune_workspaces(
            "lab-local-prune-families",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 1,
                limit: 25,
                passes: 1,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("prune apply");
        assert_eq!(applied.removed.len(), 4, "{}", prune_diagnosis(&applied));
        for removed in [&agent, &baseline, &fix_issue, &orphan_artifacts] {
            assert!(!removed.exists(), "{}", removed.display());
        }
        for removed in [&agent, &baseline, &fix_issue] {
            assert!(
                !workspace_metadata_file(removed).parent().unwrap().exists(),
                "state sibling of {} goes with its checkout",
                removed.display()
            );
        }
        for kept in [&declared, &kept, &kept_artifacts, &manual, &stray, &young] {
            assert!(kept.exists(), "{}", kept.display());
        }
    });
}

#[test]
fn ssh_orphan_artifact_dir_delete_revalidates_checkout_absence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("_lab_workspaces");
    let checkout = root.join("repo-run");
    let artifacts = root.join("repo-run-homeboy-artifacts");
    fs::create_dir_all(artifacts.join("provenance")).expect("artifacts");
    fs::create_dir_all(&checkout).expect("checkout");
    let delete = || {
        Command::new("sh")
            .arg("-c")
            .arg(ssh_prune_delete_orphan_artifact_dir_command(
                &root.display().to_string(),
                &artifacts.display().to_string(),
            ))
            .output()
            .expect("run orphan artifact delete")
    };

    let output = delete();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "unknown:artifact_dir_checkout_present"
    );
    assert!(artifacts.exists());

    fs::remove_dir_all(&checkout).expect("remove checkout");
    let output = delete();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!artifacts.exists());

    // Only `-homeboy-artifacts` siblings under the root are admissible.
    let foreign = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_orphan_artifact_dir_command(
            &root.display().to_string(),
            &checkout.display().to_string(),
        ))
        .output()
        .expect("run foreign delete");
    assert_eq!(
        String::from_utf8_lossy(&foreign.stdout),
        "unknown:workspace_path"
    );
}

#[test]
fn prune_workspaces_advances_through_thousands_of_mixed_entries() {
    crate::test_support::with_isolated_home(|_| {
        // Orphans sit behind hundreds of unowned entries, mid-root and on the
        // final entry. Thousands of fixtures only multiplied setup I/O.
        const WORKSPACE_COUNT: usize = 640;
        const ORPHAN_INDICES: [usize; 3] = [200, 401, 639];
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let workspaces_root = runner_root.path().join("_lab_workspaces");
        fs::create_dir_all(&workspaces_root).expect("workspaces root");
        // Orphans and malformed workspaces carry a `-homeboy-state` sibling
        // holding their metadata; each is its own top-level entry.
        let mut state_dir_count = 0;
        for index in 0..WORKSPACE_COUNT {
            let workspace = workspaces_root.join(format!("workspace-{index:05}"));
            if ORPHAN_INDICES.contains(&index) {
                write_orphan_workspace(&workspace);
                state_dir_count += 1;
            } else if index % 11 == 0 {
                state_dir_count += 1;
                fs::create_dir_all(&workspace).expect("malformed workspace");
                fs::create_dir_all(workspace_metadata_file(&workspace).parent().unwrap())
                    .expect("malformed workspace");
                fs::write(workspace_metadata_file(&workspace), "{malformed")
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

        // `limit` bounds candidates, not inspected entries: one cursorless
        // invocation walks every entry, plans every orphan, and reports each
        // malformed or metadata-free entry instead of silently passing it.
        let (output, _) = prune_workspaces(
            "lab-local-prune-thousands",
            RunnerWorkspacePruneOptions {
                apply: true,
                min_age_hours: 0,
                limit: 127,
                passes: 3,
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        )
        .expect("bounded mixed-entry drain");
        assert!(output.scan_complete, "{}", prune_diagnosis(&output));
        assert!(output.continuation_cursor.is_none());
        assert_eq!(
            output.scanned_workspace_count,
            WORKSPACE_COUNT + state_dir_count
        );
        assert_eq!(output.removed.len(), ORPHAN_INDICES.len());
        assert_eq!(
            output.skipped_unknown_count,
            WORKSPACE_COUNT - ORPHAN_INDICES.len()
        );
        for index in ORPHAN_INDICES {
            assert!(!workspaces_root
                .join(format!("workspace-{index:05}"))
                .exists());
        }
        assert_eq!(
            fs::read_dir(&workspaces_root)
                .expect("remaining workspaces")
                .count(),
            WORKSPACE_COUNT + state_dir_count - 2 * ORPHAN_INDICES.len()
        );
    });
}

#[test]
fn ssh_prune_scan_command_bounds_measured_rows_not_inspected_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    // Entries without ownership proof sort first and are cheap to report; the
    // row budget bounds only the size-measured rows the controller must
    // classify, so the walk passes the whole unowned prefix in one page.
    for index in 0..20 {
        fs::create_dir_all(temp.path().join(format!("manual-{index:05}"))).expect("manual dir");
    }
    for index in 0..20 {
        write_orphan_workspace(&temp.path().join(format!("workspace-{index:05}")));
    }

    let output = Command::new("sh")
        .arg("-c")
        .arg(prune_scan_command(
            &temp.path().display().to_string(),
            0,
            5,
            None,
            20,
        ))
        .output()
        .expect("run generated prune scan command");

    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("unknown\tworkspace_metadata_missing\t"))
            .count(),
        20,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("workspace\t"))
            .count(),
        5,
        "{stdout}"
    );
    // The four `-homeboy-state` siblings interleaved with the measured
    // workspaces are inspected but retained, not measured.
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("retained\tstate_dir_owned_by_checkout\t"))
            .count(),
        4,
        "{stdout}"
    );
    assert!(
        stdout.contains("__homeboy_prune_scan__\t29\tpartial"),
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
            20,
        ))
        .output()
        .expect("resume generated prune scan command");
    assert!(resumed.status.success(), "{resumed:?}");
    let resumed_stdout = String::from_utf8_lossy(&resumed.stdout);
    assert!(!resumed_stdout.contains("workspace-00004\t"));
    assert!(resumed_stdout.contains("workspace-00005\t"));
    assert!(resumed_stdout.contains("workspace-00009\t"));
    assert!(
        resumed_stdout.contains("__homeboy_prune_scan__\t10\tpartial"),
        "{resumed_stdout}"
    );
}

#[test]
fn prune_size_command_bounds_each_measurement() {
    let temp = tempfile::tempdir().expect("tempdir");
    let first = temp.path().join("workspace-00000");
    let second = temp.path().join("workspace-00001");
    write_orphan_workspace(&first);
    write_orphan_workspace(&second);
    let commands = tempfile::tempdir().expect("fake command tempdir");
    let bin = commands.path().join("bin");
    fs::create_dir_all(&bin).expect("fake command dir");
    fs::write(bin.join("du"), "#!/bin/sh\n/bin/sleep 5\nexit 1\n").expect("fake du");
    Command::new("chmod")
        .args(["+x", &bin.join("du").display().to_string()])
        .status()
        .expect("make fake du executable");
    let paths = [first.display().to_string(), second.display().to_string()];
    let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();

    let started = Instant::now();
    let output = Command::new("sh")
        .arg("-c")
        .arg(prune_size_command(&paths))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
        )
        .output()
        .expect("run generated prune size command");

    assert!(output.status.success(), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
    // A timed-out measurement reports an empty size, never a guessed one.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("\t{}\n\t{}\n", paths[0], paths[1])
    );

    let measured = Command::new("sh")
        .arg("-c")
        .arg(prune_size_command(&paths))
        .output()
        .expect("run real prune size command");
    let measured = String::from_utf8_lossy(&measured.stdout);
    for line in measured.lines() {
        let (blocks, _) = line.split_once('\t').expect("size row");
        assert!(blocks.parse::<u64>().is_ok(), "{measured}");
    }
}

#[test]
fn unavailable_job_authority_fails_closed_despite_inactive_process_evidence() {
    crate::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("workspace root");
        let workspace = root.path().join("_lab_workspaces/orphan");
        write_orphan_workspace(&workspace);
        crate::create(
            &format!(
                r#"{{"id":"lab-local-prune-advisory-size","kind":"local","workspace_root":"{}"}}"#,
                root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let runner = crate::load("lab-local-prune-advisory-size").expect("load runner");
        let mut metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(workspace_metadata_file(&workspace)).expect("metadata"),
        )
        .expect("metadata json");

        let evidence =
            workspace_liveness_with_size_observation(&runner, &metadata, &workspace, false);

        assert_eq!(evidence.state, "inactive", "{evidence:#?}");
        assert_eq!(
            evidence.observations,
            vec!["workspace_size_measurement_unavailable"]
        );

        metadata["job_id"] = serde_json::json!("active-job");
        metadata["resource_lifecycle"] = serde_json::json!({ "status": "terminal" });
        let evidence =
            workspace_liveness_with_size_observation(&runner, &metadata, &workspace, false);
        assert_eq!(evidence.state, "unknown");
        assert_eq!(
            evidence.observations,
            vec![
                "runner_job_probe_failed",
                "workspace_size_measurement_unavailable"
            ]
        );
    });
}

#[test]
fn ssh_prune_scan_command_handles_paths_that_need_shell_quoting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root's (quoted) workspaces");
    let workspace = root.join("repo's odd (name) with spaces");
    fs::create_dir_all(&workspace).expect("workspace metadata dir");
    fs::create_dir_all(workspace_metadata_file(&workspace).parent().unwrap())
        .expect("workspace metadata dir");
    fs::write(workspace.join("file.txt"), "orphan\n").expect("workspace file");
    fs::write(
        workspace_metadata_file(&workspace),
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
        .arg(prune_scan_command(
            &root.display().to_string(),
            0,
            10,
            None,
            20,
        ))
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
    assert!(
        stdout.contains("retained\tstate_dir_owned_by_checkout\t"),
        "{stdout}"
    );
    assert!(stdout.lines().count() == 3, "{stdout}");
    assert!(
        stdout.contains("__homeboy_prune_scan__\t2\tcomplete"),
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
        .arg(ssh_prune_delete_command_with_terminal_authority(
            &root.path().join("_lab_workspaces").display().to_string(),
            &workspace.display().to_string(),
            None,
            None,
        ))
        .output()
        .expect("run atomic SSH-shaped prune delete");

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!workspace.exists());

    let active = root.path().join("_lab_workspaces/active");
    write_orphan_workspace(&active);
    let metadata_path = workspace_metadata_file(&active);
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
            .expect("metadata json");
    metadata["resource_lifecycle"] = serde_json::json!({ "status": "active" });
    fs::write(&metadata_path, metadata.to_string()).expect("write active metadata");

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_authority(
            &root.path().join("_lab_workspaces").display().to_string(),
            &active.display().to_string(),
            None,
            None,
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
fn ssh_materialized_workspace_delete_requires_exact_inactive_lifecycle() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspaces = root.path().join("_lab_workspaces");
    let eligible = workspaces.join("eligible");
    write_materialized_workspace(&eligible);
    let metadata_path = workspace_metadata_file(&eligible);
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
            .expect("metadata json");
    metadata["large_comparison_payload"] = serde_json::json!("x".repeat(3 * 1024 * 1024));
    fs::write(&metadata_path, metadata.to_string()).expect("write large metadata");
    let expected_metadata = workspace_metadata(&eligible);

    let output = run_materialized_workspace_delete(&workspaces, &eligible, &expected_metadata);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!eligible.exists());

    let ambiguous = workspaces.join("ambiguous");
    write_materialized_workspace(&ambiguous);
    let expected_metadata = workspace_metadata(&ambiguous);
    let metadata_path = workspace_metadata_file(&ambiguous);
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
            .expect("metadata json");
    metadata["run_id"] = serde_json::json!("unexpected-owner");
    fs::write(&metadata_path, metadata.to_string()).expect("write ambiguous metadata");
    assert!(!encoded_materialized_workspace_metadata_is_valid(
        &encoded_workspace_metadata(&ambiguous),
        &ambiguous,
    ));
    assert!(!encoded_materialized_workspace_metadata_is_valid(
        &base64::engine::general_purpose::STANDARD.encode(b"{malformed"),
        &ambiguous,
    ));
    let output = run_materialized_workspace_delete(&workspaces, &ambiguous, &expected_metadata);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "unknown:materialized_workspace_lifecycle_changed"
    );
    assert!(ambiguous.exists());

    let malformed = workspaces.join("malformed");
    write_materialized_workspace(&malformed);
    let expected_metadata = workspace_metadata(&malformed);
    fs::write(workspace_metadata_file(&malformed), "{malformed").expect("write malformed metadata");
    let output = run_materialized_workspace_delete(&workspaces, &malformed, &expected_metadata);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "unknown:materialized_workspace_lifecycle_changed"
    );
    assert!(malformed.exists());

    let process_owned = workspaces.join("process-owned");
    write_materialized_workspace(&process_owned);
    let expected_metadata = workspace_metadata(&process_owned);
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .current_dir(&process_owned)
        .spawn()
        .expect("hold materialized workspace cwd");
    let output = run_materialized_workspace_delete(&workspaces, &process_owned, &expected_metadata);
    child.kill().expect("stop held process");
    child.wait().expect("reap held process");
    assert!(output.status.success(), "{output:?}");
    assert_ne!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(process_owned.exists());
}

#[test]
fn snapshot_git_materialized_workspace_uses_the_same_bounded_lifecycle_contract() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("_lab_workspaces/snapshot-git");
    write_materialized_workspace(&workspace);
    let metadata_path = workspace_metadata_file(&workspace);
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("metadata"))
            .expect("metadata json");
    metadata["sync_mode"] = serde_json::json!("snapshot-git");
    fs::write(&metadata_path, metadata.to_string()).expect("write snapshot-git metadata");

    assert!(encoded_materialized_workspace_metadata_is_valid(
        &encoded_workspace_metadata(&workspace),
        &workspace,
    ));
}

#[test]
fn ssh_terminal_lifecycle_delete_revalidates_process_ownership() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("_lab_workspaces/terminal");
    write_orphan_workspace(&workspace);
    let metadata_path = workspace_metadata_file(&workspace);
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
        .arg(ssh_prune_delete_command_with_terminal_authority(
            &root.path().join("_lab_workspaces").display().to_string(),
            &workspace.display().to_string(),
            Some("terminal-run"),
            None,
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
        workspace_metadata_file(&terminal),
        serde_json::to_string_pretty(&active_lifecycle_metadata("terminal-run", "terminal-run"))
            .expect("pretty terminal metadata"),
    )
    .expect("write terminal lifecycle metadata");

    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_authority(
            &workspaces.display().to_string(),
            &terminal.display().to_string(),
            Some("terminal-run"),
            None,
        ))
        .output()
        .expect("delete terminal-owned workspace");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "removed");
    assert!(!terminal.exists());

    let ambiguous = workspaces.join("ambiguous");
    write_orphan_workspace(&ambiguous);
    fs::write(
        workspace_metadata_file(&ambiguous),
        active_lifecycle_metadata("terminal-run", "other-run").to_string(),
    )
    .expect("write ambiguous lifecycle metadata");
    let output = Command::new("sh")
        .arg("-c")
        .arg(ssh_prune_delete_command_with_terminal_authority(
            &workspaces.display().to_string(),
            &ambiguous.display().to_string(),
            Some("terminal-run"),
            None,
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
        validation_dependency_ids: None,
        run_isolation_token: None,
    }
}

fn write_orphan_workspace(path: &Path) {
    fs::create_dir_all(&path).expect("workspace metadata dir");
    fs::create_dir_all(workspace_metadata_file(path).parent().unwrap())
        .expect("workspace metadata dir");
    fs::write(path.join("file.txt"), "orphan\n").expect("workspace file");
    fs::write(
        workspace_metadata_file(path),
        serde_json::json!({
            "schema": "homeboy/runner-workspace/v1",
            "local_path": path.join("missing-source").display().to_string(),
        })
        .to_string(),
    )
    .expect("workspace metadata");
}

fn write_terminal_workspace(path: &Path, source: &Path, runner_root: &Path, run_id: &str) {
    fs::create_dir_all(&path).expect("workspace metadata dir");
    fs::create_dir_all(workspace_metadata_file(path).parent().unwrap())
        .expect("workspace metadata dir");
    fs::write(path.join("file.txt"), "retained\n").expect("workspace file");
    fs::write(
        workspace_metadata_file(path),
        serde_json::json!({
            "schema": "homeboy/runner-workspace/v1",
            "run_id": run_id,
            "local_path": source.display().to_string(),
            "remote_path": path.display().to_string(),
            "resource_lifecycle": {
                "owner": "runner.workspace",
                "run_id": run_id,
                "runner_id": null,
                "path": path.display().to_string(),
                "root_bound": runner_root.join("_lab_workspaces").display().to_string(),
                "kind": "runner_workspace",
                "ttl": null,
                "cleanup_policy": "delete_on_success",
                "evidence_retention": "metadata",
                "cleanup_intent": "dry_run",
                "cleanup_command": null,
                "status": "active"
            }
        })
        .to_string(),
    )
    .expect("workspace metadata");
}

fn write_materialized_workspace(path: &Path) {
    write_orphan_workspace(path);
    fs::write(
        workspace_metadata_file(path),
        serde_json::json!({
            "schema": "homeboy/runner-workspace/v1",
            "runner_id": "lab-ssh-prune-materialized",
            "local_path": "/existing/source",
            "remote_path": path.display().to_string(),
            "sync_mode": "snapshot",
            "snapshot_identity": "snapshot:synthetic",
            "resource_lifecycle": {
                "owner": "runner.workspace",
                "run_id": "materialized-workspace",
                "path": path.display().to_string(),
                "kind": "runner_workspace",
                "cleanup_policy": "delete_on_success",
                "status": "active"
            }
        })
        .to_string(),
    )
    .expect("materialized workspace metadata");
}

fn encoded_workspace_metadata(path: &Path) -> String {
    base64::engine::general_purpose::STANDARD.encode(workspace_metadata(path))
}

fn workspace_metadata(path: &Path) -> Vec<u8> {
    fs::read(workspace_metadata_file(path)).expect("read workspace metadata for compare")
}

fn run_materialized_workspace_delete(
    root: &Path,
    workspace: &Path,
    expected_metadata: &[u8],
) -> std::process::Output {
    let command = ssh_prune_delete_materialized_workspace_command(
        &root.display().to_string(),
        &workspace.display().to_string(),
    );
    assert!(
        command.len() < 16 * 1024,
        "comparison data must not be embedded in argv"
    );
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start materialized workspace delete");
    child
        .stdin
        .take()
        .expect("delete stdin")
        .write_all(expected_metadata)
        .expect("stream expected metadata");
    child.wait_with_output().expect("finish workspace delete")
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
