//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskExecutionHandle,
    AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence, AgentTaskWorkflowStepStatus,
    AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use homeboy_core::api_jobs::{Job, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;
use sha2::{Digest, Sha256};
use std::process::Command;
use std::sync::Mutex;

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> AgentTaskLifecycleStore {
    AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store")
}

enum TestRunnerReconciliation {
    Snapshot(Box<homeboy_core::api_jobs::RunnerJobLogSnapshot>),
    ConfirmedAbsent(usize),
    Unconfirmed,
}

struct ReconciliationProvider {
    result: Mutex<Option<TestRunnerReconciliation>>,
    recovered_runner_job_id: Mutex<Option<String>>,
}

impl RunnerContinuationProvider for ReconciliationProvider {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        Err(Error::internal_unexpected(
            "snapshot must use generation reconciliation",
        ))
    }

    fn reconcile_runner_job(&self, _runner_id: &str, _job_id: &str) -> RunnerJobReconciliation {
        match self.result.lock().expect("reconciliation result").take() {
            Some(TestRunnerReconciliation::Snapshot(snapshot)) => {
                RunnerJobReconciliation::Snapshot(snapshot)
            }
            Some(TestRunnerReconciliation::ConfirmedAbsent(checked_generations)) => {
                RunnerJobReconciliation::ConfirmedAbsent {
                    checked_generations,
                }
            }
            Some(TestRunnerReconciliation::Unconfirmed) | None => {
                RunnerJobReconciliation::UnconfirmedAbsence
            }
        }
    }

    fn runner_job_id_for_durable_run(
        &self,
        _runner_id: &str,
        _durable_run_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .recovered_runner_job_id
            .lock()
            .expect("recovered runner job")
            .clone())
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        true
    }

    fn runner_authority(&self, _runner_id: &str) -> RunnerAuthority {
        RunnerAuthority::Configured
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        Err(Error::internal_unexpected("not used by reconciliation"))
    }

    fn submit_reverse_broker_job(
        &self,
        _runner_id: &str,
        _request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        Err(Error::internal_unexpected("not used by reconciliation"))
    }
}

fn accepted_detached_handoff(run_id: &str) -> AgentTaskRunRecord {
    let plan = test_plan();
    record_lab_offload_phase(
        run_id,
        "homeboy-lab",
        "lab_handoff_preacceptance",
        Some("/runner/workspace/homeboy"),
        None,
        None,
        Some(&plan),
    )
    .expect("persist pending handoff");
    record_detached_lab_run(DetachedLabRunRecord {
        run_id,
        runner_id: "homeboy-lab",
        runner_job_id: "00000000-0000-0000-0000-000000000123",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &[],
    })
    .expect("accept handoff")
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Acceptance is a durable transfer of one run to one runner daemon:
/// the pending handoff, the typed acceptance, the reload that models caller
/// loss, and the snapshot validated against it all name `lifecycle_store`, so
/// "the accepted identity survived" is asserted about one home rather than
/// about whichever home the process environment happened to point at.
///
/// `bind_accepted_lab_runner_job_in_store` still carries the default Lab
/// offload submission, but it cannot be reached here: the pending handoff above
/// has already written the record, so the acceptance path reads it rather than
/// falling through to `submit_plan_in_store`.
#[test]
fn accepted_runner_identity_binds_before_snapshot_validation_and_survives_caller_loss() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-9567-pre-provider-race";
    let plan = test_plan();
    record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "provider_dispatch",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist pending handoff before daemon acceptance");
    let identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
        run_id,
        "homeboy-lab",
        "00000000-0000-0000-0000-000000009567",
    );

    // Daemon admission persists the typed accepted identity before a
    // foreground caller can observe a snapshot or disappear.
    let accepted = bind_accepted_lab_runner_job_in_store(
        &lifecycle_store,
        &identity,
        "/runner/workspace/homeboy",
        &[],
    )
    .expect("bind accepted daemon job");
    let replay = bind_accepted_lab_runner_job_in_store(
        &lifecycle_store,
        &identity,
        "/runner/workspace/homeboy",
        &[],
    )
    .expect("repeated acceptance is idempotent");
    assert_eq!(accepted, replay);

    let foreign = bind_accepted_lab_runner_job_in_store(
        &lifecycle_store,
        &homeboy_core::lab_contract::RunnerJobIdentity::new(
            run_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000009568",
        ),
        "/runner/workspace/homeboy",
        &[],
    )
    .expect_err("a foreign daemon job cannot replace accepted identity");
    assert_eq!(foreign.code, ErrorCode::ValidationInvalidArgument);

    // Reloading models controller/caller loss after daemon acceptance. A
    // status reconciliation validates the accepted job directly and never
    // needs to replay the provider command.
    let mut recovered = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("reload accepted handoff")
    .record;
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&plan));
    snapshot.job.id = uuid::Uuid::parse_str(&identity.runner_job_id).expect("valid job id");
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events.clear();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut recovered, &snapshot)
        .expect("accepted identity validates recovered runner snapshot");
    assert_eq!(
        recovered.runner_job_id(),
        Some(identity.runner_job_id.as_str())
    );
    assert_eq!(recovered.state, AgentTaskRunState::Running);
}

#[test]
fn accepted_runner_binding_fails_if_terminalization_wins_before_identity_persistence() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-12926-accepted-before-binding";
    let plan = test_plan();
    record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "provider_dispatch",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist planned handoff before daemon acceptance");
    let mut terminal = lifecycle_store
        .read_record(run_id)
        .expect("planned handoff");
    set_run_state(&mut terminal, AgentTaskRunState::Cancelled);
    terminal.metadata["cancel_reason"] = json!("missing_runner_pid");
    lifecycle_store
        .write_record(&terminal)
        .expect("terminalization wins before accepted identity is bound");
    let identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
        run_id,
        "homeboy-lab",
        "00000000-0000-0000-0000-000000012926",
    );

    let error = bind_accepted_lab_runner_job_in_store(
        &lifecycle_store,
        &identity,
        "/runner/workspace/homeboy",
        &[],
    )
    .expect_err("accepted work cannot proceed without a durable runner identity");

    assert_eq!(error.code, ErrorCode::InternalUnexpected);
    let persisted = lifecycle_store
        .read_record(run_id)
        .expect("terminal record remains readable");
    assert_eq!(persisted.state, AgentTaskRunState::Cancelled);
    assert!(accepted_lab_runner_job_identity_from_record(&persisted).is_none());
    assert!(persisted.runner_job_id().is_none());
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The mutable metadata is written into, and the typed-handoff read is
/// taken out of, one store — which is the whole claim: "metadata is not
/// acceptance" is only meaningful when the read half and the write half name
/// the same record. The stub admission keeps submission off the machine-global
/// controller-runtime queue; nothing here asserts on runtime provenance.
#[test]
fn accepted_runner_identity_rejects_mutable_metadata_without_typed_handoff() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-9567-metadata-is-not-acceptance";
    let mut record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("persist run");
    record.metadata["runner_id"] = json!("homeboy-lab");
    record.metadata["runner_job_id"] = json!("foreign-job");
    lifecycle_store
        .write_record(&record)
        .expect("persist mutable metadata");

    assert!(
        accepted_lab_runner_job_identity_in_store(&lifecycle_store, run_id)
            .expect("read typed handoff")
            .is_none()
    );
}

#[test]
fn submit_plan_persists_queued_status() {
    with_isolated_home(|_| {
        let plan = test_plan();

        let record = submit_plan(&plan, Some("run/a")).expect("submitted");
        let loaded = reconcile_status(&record.run_id).expect("status loaded");

        assert_eq!(record.run_id, "run_a");
        assert_eq!(loaded.state, AgentTaskRunState::Queued);
        assert_eq!(
            loaded.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
                ["requested"],
            homeboy_core::build_identity::current().display
        );
        assert!(
            loaded.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
                ["originating"]["pinned_executable"]
                .as_str()
                .is_some()
        );
        assert_eq!(loaded.tasks[0].task_id, "task-a");
        assert_eq!(
            loaded.tasks[0].provider_ref.as_deref(),
            Some("test:fixture")
        );
    });
}

#[test]
fn submit_plan_persists_safe_route_resolution_without_a_destination() {
    with_isolated_home(|_| {
        let mut resolution =
            homeboy_core::notification_route::NotificationRouteResolution::new("route_less");
        resolution.resolver_transport = Some("generic.completed".to_string());
        resolution.missing_context = vec!["CALLER_THREAD_ID".to_string()];

        let record = homeboy_core::notification_route::with_current_resolution(
            Some(resolution.clone()),
            || submit_plan(&test_plan(), Some("route-less-cook")).expect("submitted"),
        );

        assert_eq!(
            record.metadata["notification_resolution"],
            serde_json::to_value(resolution).unwrap()
        );
        assert!(record.metadata.get("notification_route").is_none());
        assert!(!serde_json::to_string(&record.metadata)
            .unwrap()
            .contains("opaque-destination"));
    });
}

#[cfg(unix)]
fn pin_record_from_artifact(
    run_id: &str,
    artifact: &std::path::Path,
    digest: &str,
    identity: &str,
) {
    let temporary_legacy = artifact
        .parent()
        .expect("artifact parent")
        .join(format!("{run_id}-legacy"));
    std::fs::write(&temporary_legacy, b"corrupted legacy bytes").expect("write legacy pin");
    rewrite_record_for_test(run_id, |record| {
        record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
            "originating": {
                "build_identity": identity,
                "pinned_executable": temporary_legacy,
                "sha256": digest,
            }
        });
    })
    .expect("project legacy pin");
    recover_controller_runtime_in_store(&test_lifecycle_store(), run_id, Some(artifact), None)
        .expect("recover pin");
}

#[cfg(unix)]
fn record_pin(run_id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(
        reconcile_status(run_id).expect("record").metadata
            [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
            ["pinned_executable"]
            .as_str()
            .expect("pin"),
    )
}

#[cfg(unix)]
fn snapshot_reasons_for(
    snapshots: &[homeboy_core::controller_runtime::ControllerRuntimeSnapshot],
    pin: &std::path::Path,
) -> Vec<String> {
    snapshots
        .iter()
        .find(|snapshot| snapshot.pins.iter().any(|candidate| candidate == pin))
        .map(|snapshot| snapshot.retention_reasons.clone())
        .expect("snapshot")
}

#[cfg(unix)]
#[test]
fn controller_pin_retention_keeps_in_flight_and_pending_mutation_inside_the_window() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        super::controller_pin_reference_provider::register();
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let active_artifact = temporary.path().join("active-homeboy");
        let terminal_artifact = temporary.path().join("terminal-homeboy");
        let active_digest =
            fake_controller_artifact(&active_artifact, &identity, "active artifact");
        let terminal_digest =
            fake_controller_artifact(&terminal_artifact, &identity, "terminal artifact");

        let active = submit_plan(&test_plan(), Some("retention-active")).expect("submit active");
        let terminal =
            submit_plan(&test_plan(), Some("retention-terminal")).expect("submit terminal");
        pin_record_from_artifact(&active.run_id, &active_artifact, &active_digest, &identity);
        pin_record_from_artifact(
            &terminal.run_id,
            &terminal_artifact,
            &terminal_digest,
            &identity,
        );
        rewrite_record_for_test(&terminal.run_id, |record| {
            set_run_state(record, AgentTaskRunState::Succeeded);
        })
        .expect("make terminal");

        let active_pin = record_pin(&active.run_id);
        let terminal_pin = record_pin(&terminal.run_id);
        let report =
            homeboy_core::controller_runtime::retention_report().expect("retention report");
        assert!(report.retained.contains(&active_pin));
        assert!(report.retained.contains(&terminal_pin));
        assert!(snapshot_reasons_for(&report.snapshots, &active_pin)
            .iter()
            .any(|reason| reason == "protected_in_flight"));
        assert!(snapshot_reasons_for(&report.snapshots, &terminal_pin)
            .iter()
            .any(|reason| reason == "protected_by_pending_mutation"));
        let purge = homeboy_core::controller_runtime::ControllerRuntimeRetentionOverrides {
            limit: None,
            ignore_retention: true,
        };
        let applied = prune_controller_runtime_pins(true, purge).expect("prune unreferenced pins");
        assert!(!applied.removed.contains(&terminal_pin));
        assert!(active_pin.exists());
        assert!(terminal_pin.exists());
    });
}

#[cfg(unix)]
#[test]
fn controller_pin_retention_reclaims_old_terminal_retained_artifacts_under_pressure() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        super::controller_pin_reference_provider::register();
        homeboy_core::defaults::save_config(&homeboy_core::defaults::HomeboyConfig {
            retention: homeboy_core::defaults::RetentionConfig {
                controller_runtime_days: 14,
                controller_runtime_max_bytes: 0,
                limit: 10,
                ..homeboy_core::defaults::RetentionConfig::default()
            },
            ..homeboy_core::defaults::HomeboyConfig::default()
        })
        .expect("save retention config");
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let terminal_artifact = temporary.path().join("terminal-homeboy");
        let terminal_digest =
            fake_controller_artifact(&terminal_artifact, &identity, "terminal artifact");
        let terminal =
            submit_plan(&test_plan(), Some("retention-old-terminal")).expect("submit terminal");
        pin_record_from_artifact(
            &terminal.run_id,
            &terminal_artifact,
            &terminal_digest,
            &identity,
        );
        rewrite_record_for_test(&terminal.run_id, |record| {
            set_run_state(record, AgentTaskRunState::Succeeded);
            record.lifecycle.artifact_retention.status = ArtifactRetentionStatus::Retained;
            record.submitted_at = "2020-01-01T00:00:00Z".to_string();
        })
        .expect("age terminal retained record");

        let terminal_pin = record_pin(&terminal.run_id);
        let applied = prune_controller_runtime_pins(
            true,
            homeboy_core::controller_runtime::ControllerRuntimeRetentionOverrides::default(),
        )
        .expect("reclaim under pressure");
        assert!(applied.removed.contains(&terminal_pin));
        assert!(!terminal_pin.exists());
        assert!(applied.snapshots.iter().any(|snapshot| {
            snapshot.pins.contains(&terminal_pin)
                && snapshot
                    .retention_reasons
                    .iter()
                    .any(|reason| reason == "reclaimable")
        }));
    });
}

#[cfg(unix)]
#[test]
fn controller_pin_retention_keeps_queued_runs_outside_the_age_window() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        super::controller_pin_reference_provider::register();
        homeboy_core::defaults::save_config(&homeboy_core::defaults::HomeboyConfig {
            retention: homeboy_core::defaults::RetentionConfig {
                controller_runtime_days: 0,
                controller_runtime_max_bytes: 0,
                limit: 10,
                ..homeboy_core::defaults::RetentionConfig::default()
            },
            ..homeboy_core::defaults::HomeboyConfig::default()
        })
        .expect("save retention config");
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let queued_artifact = temporary.path().join("queued-homeboy");
        let queued_digest =
            fake_controller_artifact(&queued_artifact, &identity, "queued artifact");
        let queued =
            submit_plan(&test_plan(), Some("retention-old-queued")).expect("submit queued");
        pin_record_from_artifact(&queued.run_id, &queued_artifact, &queued_digest, &identity);
        rewrite_record_for_test(&queued.run_id, |record| {
            record.submitted_at = "2020-01-01T00:00:00Z".to_string();
        })
        .expect("age queued record");

        let queued_pin = record_pin(&queued.run_id);
        let report =
            homeboy_core::controller_runtime::retention_report().expect("retention report");
        assert!(report.retained.contains(&queued_pin));
        assert!(snapshot_reasons_for(&report.snapshots, &queued_pin)
            .iter()
            .any(|reason| reason == "protected_in_flight"));
        let applied = prune_controller_runtime_pins(
            true,
            homeboy_core::controller_runtime::ControllerRuntimeRetentionOverrides::default(),
        )
        .expect("prune");
        assert!(!applied.removed.contains(&queued_pin));
        assert!(queued_pin.exists());
    });
}

#[test]
fn active_pinned_run_does_not_block_controller_promotion() {
    with_isolated_home(|_| {
        submit_plan(&test_plan(), Some("active-pinned-runtime")).expect("submitted");

        // Promotion no longer drains durable work. The record owns its pinned
        // runtime and remains available while later admissions switch.
        homeboy_core::controller_runtime::activate_current_generation()
            .expect("active durable run must not block promotion");
        let after = submit_plan(&test_plan(), Some("post-promotion-runtime"))
            .expect("post-switch submission");
        assert_eq!(after.state, AgentTaskRunState::Queued);
    });
}

// Stamp a durable run's controller-runtime metadata with an obsolete build
// identity, simulating a run created before a controller/runner upgrade.
fn stamp_stale_controller_runtime(run_id: &str, stale_identity: &str) {
    rewrite_record_for_test(run_id, |record| {
        record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
            "schema": "homeboy/controller-runtime-pin/v2",
            "requested": stale_identity,
            "originating": {
                "build_identity": stale_identity,
                "executable": "/legacy/homeboy",
                "pinned_executable": "/legacy/homeboy",
                "sha256": "0".repeat(64),
            },
            "current": stale_identity,
            "executed": stale_identity,
        });
    })
    .expect("stamp stale controller runtime");
}

fn stamped_runtime_identity(run_id: &str) -> String {
    reconcile_status(run_id).expect("record loaded").metadata
        [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
        ["build_identity"]
        .as_str()
        .expect("stamped build identity")
        .to_string()
}

#[test]
fn retry_stamps_replacement_run_with_current_runtime_not_stale_source() {
    // #8550: a Lab cook created under an older controller runtime left the durable
    // run pinned to that obsolete build. After controller and runner were upgraded
    // to the same current build, a clean lifecycle retry produced a fresh run ID
    // but retained the obsolete runtime provenance, so the runner refused it with
    // `Invalid argument controller_runtime`. A replacement run must be owned by the
    // runtime that creates it.
    with_isolated_home(|_| {
        let plan = test_plan();
        let current_identity = homeboy_core::build_identity::current().display;
        let stale_identity = format!("{current_identity}-obsolete-predecessor");
        assert_ne!(stale_identity, current_identity);

        let source = submit_plan(&plan, Some("cook-8550-source")).expect("source submitted");
        stamp_stale_controller_runtime(&source.run_id, &stale_identity);
        assert_eq!(stamped_runtime_identity(&source.run_id), stale_identity);

        // (1) A failed run created by runtime A can be retried with a new run ID
        //     under runtime B, and the replacement run records runtime B.
        let replacement = retry(&source.run_id, Some("cook-8550-retry")).expect("retry succeeds");
        assert_ne!(replacement.run_id, source.run_id);
        assert_eq!(
            stamped_runtime_identity(&replacement.run_id),
            current_identity,
            "replacement run must be stamped with the current runtime that created it"
        );
        assert_eq!(
            replacement.metadata["retry_of"].as_str(),
            Some(source.run_id.as_str())
        );

        // (2) Mutating the original runtime-A run under runtime B remains rejected.
        let source_record = reconcile_status(&source.run_id).expect("source record");
        let mutation = homeboy_core::controller_runtime::validate_for_mutation(
            &source_record.metadata,
            &current_identity,
        );
        assert!(
            mutation.is_err(),
            "mutating the stale source run under the current runtime must stay fail-closed"
        );

        // (3) A same-runtime retry retains current behavior: the replacement is
        //     owned by the current runtime and the source is untouched.
        let same_runtime_source =
            submit_plan(&plan, Some("cook-8550-fresh")).expect("fresh source submitted");
        assert_eq!(
            stamped_runtime_identity(&same_runtime_source.run_id),
            current_identity
        );
        let same_runtime_replacement =
            retry(&same_runtime_source.run_id, None).expect("same-runtime retry succeeds");
        assert_eq!(
            stamped_runtime_identity(&same_runtime_replacement.run_id),
            current_identity
        );
        assert_eq!(
            stamped_runtime_identity(&same_runtime_source.run_id),
            current_identity,
            "retry must not rewrite the source run's runtime provenance"
        );
    });
}

#[test]
fn retry_rebuilds_follow_up_candidate_from_durable_promotion() {
    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary repository");
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).expect("create repository");
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init"]);
        std::fs::write(repo.join("candidate.txt"), "base\n").expect("write base");
        git(&["add", "candidate.txt"]);
        git(&[
            "-c",
            "user.name=Homeboy Test",
            "-c",
            "user.email=homeboy-test@localhost",
            "commit",
            "-m",
            "base",
        ]);
        let head = git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.join("candidate.txt"), "candidate\n").expect("write candidate");
        let patch = format!("{}\n", git(&["diff", "--binary"]));
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, &patch).expect("write patch artifact");
        let patch_sha = format!("{:x}", Sha256::digest(patch.as_bytes()));

        let source_run = "cook-follow-up-source";
        let mut plan = test_plan();
        plan.tasks[0].inputs = json!({
            "cook_loop": {
                "artifact_provenance": {
                    "source_run_id": source_run,
                    "source_task_id": "task-a",
                    "source_patch_artifact_sha256": patch_sha,
                }
            }
        });
        plan.tasks[0].workspace.root = Some("/stale/follow-up-baseline".to_string());
        submit_plan(&plan, Some(source_run)).expect("submit source run");
        record_promotion(
            source_run,
            json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "gate_failed",
                "source": {"kind": "aggregate", "task_id": "task-a", "run_id": source_run},
                "to_worktree": "homeboy@test",
                "target": {"worktree": "homeboy@test", "path": repo, "head": head},
                "patch_artifact": {
                    "id": "patch",
                    "kind": "patch",
                    "path": patch_path,
                    "sha256": patch_sha,
                },
                "provenance": {
                    "worktree_path": repo,
                    "gate_feedback_baseline": {"current_diff": patch},
                },
                "operator_notification": {"status": "completed", "message": "complete"},
            }),
        )
        .expect("record durable promotion");

        let replacement = retry(source_run, Some("cook-follow-up-retry")).expect("retry succeeds");
        let restored = load_plan(&replacement.run_id).expect("load replacement plan");
        let restored_task = &restored.tasks[0];
        let restored_root = restored_task
            .workspace
            .root
            .as_deref()
            .expect("restored root");
        assert_ne!(restored_root, "/stale/follow-up-baseline");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(restored_root).join("candidate.txt"))
                .expect("read restored candidate"),
            "candidate\n"
        );
        assert_eq!(
            restored_task.metadata["verified_cook_baseline"]["source_run_id"],
            source_run
        );
        assert_eq!(
            restored_task.metadata["verified_cook_baseline"]["promoted_patch_artifact_sha256"],
            patch_sha
        );

        git(&["worktree", "remove", "--force", restored_root]);
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The planned proxy, the plan and log projections read back off it,
/// and the accepted child that binds it are one operation on one home — the
/// binding is only evidence of anything if the record it advances is the record
/// the proxy was written into. The acceptance below cannot reach the default
/// Lab-offload submission because the planned proxy has already written the
/// record.
#[test]
fn controller_proxy_is_queued_before_handoff_then_binds_runner_child() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    let planned = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "agent-task-controller-proxy",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("controller proxy recorded before handoff");

    assert_eq!(planned.state, AgentTaskRunState::Queued);
    assert!(planned.metadata.get("runner_job_id").is_none());
    assert_eq!(planned.metadata["lifecycle_store_owner"], "controller");
    assert!(planned.lab_handoff.is_none());
    assert!(planned.metadata.get("handoff_acceptance").is_none());
    assert!(
        load_plan_in_store(&lifecycle_store, "agent-task-controller-proxy")
            .expect("proxy plan")
            .tasks[0]
            .inputs
            .get("runner_job_id")
            .is_none()
    );
    assert_eq!(
        planned.metadata["runner_execution_record"]["status"],
        "planned"
    );
    assert_eq!(
        logs_in_store(&lifecycle_store, "agent-task-controller-proxy")
            .expect("logs resolve")
            .events
            .len(),
        1
    );

    let running = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-controller-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
    )
    .expect("accepted child binds proxy");
    assert_eq!(running.state, AgentTaskRunState::Running);
    assert_eq!(running.metadata["runner_job_id"], "job-123");
    assert_eq!(running.metadata["lifecycle_store_owner"], "controller");
    assert_eq!(running.metadata["handoff_acceptance"]["state"], "accepted");
    assert_eq!(
        running.metadata["runner_execution_record"]["status"],
        "running"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The whole point of this write is that an operator can find the run
/// holding a reservation, so the reservation and the run it names have to be
/// one home — a reservation named in one installation while the run lives in
/// another is exactly the unfindable state #9163 forced manual job-ID
/// cancellation for. The cancellation half follows the same store, so "a
/// terminal run is never reopened" is asserted about the record the reservation
/// was written onto.
#[test]
fn reserved_lab_admission_is_named_on_the_durable_run_before_acceptance() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "agent-task-admission-identity",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("controller proxy recorded before admission");

    let reserved = record_lab_admission_reservation_in_store(
        &lifecycle_store,
        "agent-task-admission-identity",
        "homeboy-lab",
        "lease-abc",
        "job-reservation-9163",
        1_700_000_000_000,
    )
    .expect("reservation named on the durable run");

    // The reservation is findable by run id the moment it is taken, so a
    // caller killed before runner acceptance never leaves an admission that
    // only manual job-ID surgery can identify (#9163).
    assert_eq!(
        reserved.metadata["lab_admission_reservation"]["reservation_job_id"],
        "job-reservation-9163"
    );
    assert_eq!(
        reserved.metadata["lab_admission_reservation"]["daemon_lease_id"],
        "lease-abc"
    );
    assert_eq!(
        reserved.metadata["lab_admission_reservation"]["runner_id"],
        "homeboy-lab"
    );
    assert_eq!(
        reserved.metadata["lab_admission_reservation"]["lease_expires_at_ms"],
        1_700_000_000_000u64
    );
    assert_eq!(
        reserved.metadata["lab_admission_reservation"]["cancel_command"],
        "homeboy agent-task cancel agent-task-admission-identity"
    );
    // Naming a reservation is evidence, never acceptance.
    assert_eq!(reserved.state, AgentTaskRunState::Queued);
    assert!(reserved.metadata.get("runner_job_id").is_none());

    // A terminal run is never reopened by a late reservation write.
    cancel_run_in_store(
        &lifecycle_store,
        "agent-task-admission-identity",
        Some("caller lost"),
    )
    .expect("cancel the reserved run");
    let after_cancel = record_lab_admission_reservation_in_store(
        &lifecycle_store,
        "agent-task-admission-identity",
        "homeboy-lab",
        "lease-def",
        "job-reservation-late",
        1_700_000_000_001,
    )
    .expect("late reservation write is a no-op");
    assert_eq!(
        after_cancel.metadata["lab_admission_reservation"]["reservation_job_id"],
        "job-reservation-9163"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Double-acceptance is the failure this guards: an acceptance
/// validated against one home's record and committed into another's would let
/// two runners own the same run, and the handoff lock would have excluded
/// neither. Every acceptance below — and the read back that proves the first
/// one is retained — names `lifecycle_store`. The planned handoff writes the
/// record first, so no acceptance here reaches the default submission.
#[test]
fn accepted_handoff_replays_idempotently_and_rejects_a_different_identity() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "immutable-handoff",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("planned handoff");
    let input = DetachedLabRunRecord {
        run_id: "immutable-handoff",
        runner_id: "homeboy-lab",
        runner_job_id: "job-immutable",
        remote_workspace: "/runner/workspace/repo",
        remote_command: &command,
    };
    let accepted = record_detached_lab_run_in_store(&lifecycle_store, input.clone())
        .expect("accepted handoff");
    let replay =
        record_detached_lab_run_in_store(&lifecycle_store, input).expect("idempotent replay");
    assert_eq!(replay, accepted);

    let error = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "immutable-handoff",
            runner_id: "other-runner",
            runner_job_id: "other-job",
            remote_workspace: "/other/workspace",
            remote_command: &command,
        },
    )
    .expect_err("different accepted identity is rejected");
    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    let stored = reconcile_status_in_store(
        &lifecycle_store,
        "immutable-handoff",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("accepted record retained")
    .record;
    assert_eq!(stored.runner_id(), Some("homeboy-lab"));
    assert_eq!(stored.runner_job_id(), Some("job-immutable"));
}

/// Deliberately still on `with_isolated_home` (#7505), and not because a rooted
/// sibling is missing — `reconcile_transport_proxy_snapshot_in_store` exists and
/// every other call here has one.
///
/// The blocker is the closing `reconcile_status()`. Unlike its migrated siblings, this
/// test reads status back over a record the reconciliation left *Running* and
/// runner-backed, so `runner_probe_plan` returns `performed: true` and
/// `reconcile_status_in_store` reaches `reconcile_runner_job_state_in_store` ->
/// `with_runner_continuation`. That provider slot is process-global by design
/// (#12618) and is *not* covered by any lock of its own:
/// `RunnerContinuationTestGuard` installs and clears it, and the only thing
/// serializing installers today is the hermetic-home mutex every ambient test
/// holds.
///
/// Three ambient tests in this same file install a one-shot
/// `ReconciliationProvider` whose result is a `Mutex<Option<..>>` that
/// `reconcile_runner_job` `take()`s. A rooted form of this test would run
/// concurrently with them and consume that single result — silently failing
/// *those* tests, and taking a `ConfirmedAbsent` verdict that would terminalize
/// this run and break the `runner_job_id` assertion below. Migrating this one
/// trades a hermetic-home mutation for a genuine cross-test race, so it stays
/// until the continuation registry is serialized independently.
#[test]
fn runner_snapshot_binds_pending_lab_handoff_before_validation() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "snapshot-accepted-handoff",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("pending controller handoff");
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
        snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
        snapshot.events.clear();

        reconcile_transport_proxy_snapshot_in_store(
            &AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store"),
            &mut record,
            &snapshot,
        )
        .expect("accepted runner snapshot binds the pending handoff");

        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(
            record.runner_job_id(),
            Some("00000000-0000-0000-0000-000000000123")
        );
        assert_eq!(record.metadata["handoff_acceptance"]["state"], "accepted");
        assert_eq!(
            reconcile_status("snapshot-accepted-handoff")
                .expect("durable handoff")
                .runner_job_id(),
            Some("00000000-0000-0000-0000-000000000123")
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The planned proxy, the recovery-state write that strips the pending
/// handoff, and the reconciliation that has to bind from what is left are one
/// operation on one durable row: "the runner identity survives only on the
/// planned execution record" is a claim about a specific record, so the record
/// the fixture mutilates and the record the binder reads must be the same one.
///
/// `reconcile_transport_proxy_snapshot_in_store` is a pure alias for
/// `reconcile_runner_job_snapshot_in_store`, whose binding write, aggregate
/// idempotence read, and terminal commit are all rooted. The bind reaches
/// `record_detached_lab_run_in_store`, which still carries the default Lab
/// offload submission — but it cannot fire here, because the planned proxy has
/// already written the record, so acceptance reads it rather than falling
/// through to `submit_plan_in_store`.
#[test]
fn preacceptance_snapshot_binds_replacement_job_from_planned_execution_record() {
    // Issue #9382: a durable Lab-offloaded run interrupted *before* acceptance
    // has no accepted runner_job_id and — after deadline/recovery handling — no
    // Pending controller handoff to bind from either. Its runner identity lives
    // only in the planned `runner_execution_record`. Recovery re-executes the
    // exact run, the runner accepts a fresh replacement job, and its snapshot
    // must bind that replacement rather than reject it as "no accepted runner
    // job identity".
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "preacceptance-recovery",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("planned controller proxy");

    // Simulate the post-interruption recovery state: the pending controller
    // handoff is gone (deadline/recovery cleared it) AND the metadata
    // runner_id was not persisted, so the runner identity survives only on
    // the planned execution record. Without the #9382 fix the binder has no
    // runner source and validation rejects the replacement job.
    record.lab_handoff = None;
    record
        .metadata
        .as_object_mut()
        .expect("metadata object")
        .remove("runner_id");
    assert!(record.runner_id().is_none());
    assert_eq!(
        record.metadata["runner_execution_record"]["status"],
        "planned"
    );
    assert_eq!(
        record.metadata["runner_execution_record"]["runner_id"],
        "homeboy-lab"
    );
    lifecycle_store
        .write_record(&record)
        .expect("persist recovery state");

    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
    snapshot.events.clear();
    let replacement_job_id = snapshot.job.id.to_string();

    reconcile_transport_proxy_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("replacement runner snapshot binds the planned execution record");

    assert_eq!(record.state, AgentTaskRunState::Running);
    assert_eq!(record.runner_job_id(), Some(replacement_job_id.as_str()));
    assert_eq!(record.metadata["handoff_acceptance"]["state"], "accepted");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "The controller job" the snapshot is refused against is the one the
/// acceptance above persisted, so the acceptance and the validation that reads
/// it back name one home. The run does not exist yet, so the acceptance is
/// spelled with its explicit submission rather than the default one, which would
/// reach the machine-global controller-runtime admission queue — see
/// `stub_lab_offload_submission`.
///
/// The refusal happens before any durable write: the record already carries an
/// accepted `runner_job_id`, so `bind_pending_lab_handoff_snapshot_in_store`
/// returns immediately and `validate_runner_job_snapshot` rejects.
#[test]
fn runner_snapshot_rejects_conflicting_bound_lab_job_identity() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "snapshot-conflicting-handoff",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000456",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("accepted controller handoff");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
    snapshot.events.clear();

    let error =
        reconcile_transport_proxy_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
            .expect_err("different runner snapshot job is rejected");

    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert!(error.message.contains("does not match controller job"));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "Without mutation" is a claim about what did *not* change on disk,
/// so the record read back has to be the one the refused acceptance would have
/// mutated — the same store the pending handoff was written into.
#[test]
fn pending_handoff_rejects_acceptance_from_a_different_runner_without_mutation() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let planned = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "pending-runner-identity",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned handoff");

        let error = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "pending-runner-identity",
            runner_id: "other-runner",
            runner_job_id: "job-other",
            remote_workspace: "/other/workspace",
            remote_command: &command,
        })
        .expect_err("different runner cannot accept pending handoff");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        let stored = reconcile_status("pending-runner-identity").expect("pending handoff retained");
        assert_eq!(stored, planned);
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "Without rewriting the legacy projection" is a claim about what the
/// refused resume left on disk, so the accepted record read back has to be the
/// one the resume would have rewritten.
#[test]
fn accepted_proxy_resume_rejects_a_different_runner_without_rewriting_legacy_projection() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "immutable-proxy",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("planned handoff");
    record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "immutable-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-proxy",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
    )
    .expect("accepted handoff");

    let error = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "immutable-proxy",
            runner_id: "other-runner",
            remote_workspace: "/other/workspace",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect_err("different runner resume is rejected");
    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    let stored = reconcile_status_in_store(
        &lifecycle_store,
        "immutable-proxy",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("accepted record retained")
    .record;
    assert_eq!(stored.metadata["runner_id"], "homeboy-lab");
    assert_eq!(stored.metadata["runner_job_id"], "job-proxy");
    assert_eq!(
        stored.metadata["runner_execution_record"]["status"],
        "running"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "The same attempt advanced" is only assertable when the
/// pre-acceptance record and the accepted one are the same durable row, so the
/// phase write and the acceptance name one store.
#[test]
fn detached_cook_attempt_proxy_advances_after_daemon_acceptance() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    let attempt_run_id = "cook-7970-attempt-1-controller";
    let plan = test_plan();
    let queued = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: attempt_run_id,
            runner_id: "homeboy-lab",
            phase: "materializing",
            remote_workspace: None,
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("pre-acceptance attempt record");

    assert_eq!(queued.state, AgentTaskRunState::Queued);
    assert_eq!(queued.metadata["phase"], "materializing");
    assert!(queued.metadata.get("runner_job_id").is_none());

    let accepted = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: attempt_run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-7970",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
    )
    .expect("daemon acceptance advances the same attempt");

    assert_eq!(accepted.run_id, attempt_run_id);
    assert_eq!(accepted.state, AgentTaskRunState::Running);
    assert_eq!(accepted.metadata["runner_job_id"], "job-7970");
    assert_eq!(accepted.metadata["phase"], "awaiting_runner_result");
    assert_eq!(
        accepted.metadata["phase_activity"],
        "controller handoff complete; awaiting authoritative runner daemon result"
    );
    assert_eq!(accepted.metadata["runner_handoff"]["state"], "in_flight");
    assert!(accepted.metadata.get("runner_queue").is_none());
    assert_eq!(
        accepted.metadata["runner_handoff"]["continuation"]["intent"],
        "reconcile_runner_job"
    );
    assert_eq!(
        accepted.metadata["runner_handoff"]["identity"]["runner_job_id"],
        "job-7970"
    );
    assert_eq!(
        accepted.metadata["runner_execution_record"]["status"],
        "running"
    );
}

#[test]
fn accepted_handoff_adopts_a_job_found_on_another_known_generation() {
    with_isolated_home(|_| {
        let run_id = "generation-move-recovery";
        accepted_detached_handoff(run_id);
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
        snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
        snapshot.events.clear();
        let _provider = RunnerContinuationTestGuard::install(Box::new(ReconciliationProvider {
            // This represents a 404 on the current generation followed by a
            // matching snapshot on another generation in the durable ledger.
            result: Mutex::new(Some(TestRunnerReconciliation::Snapshot(Box::new(snapshot)))),
            recovered_runner_job_id: Mutex::new(None),
        }));

        let reconciled = reconcile_status(run_id).expect("adopted generation snapshot");

        assert_eq!(reconciled.state, AgentTaskRunState::Running);
        assert_eq!(reconciled.metadata["runner_job_status"], "running");
        assert_eq!(reconciled.metadata["provider_executions_consumed"], 0);
        assert!(reconciled.provider_handles.is_empty());
    });
}

#[test]
fn accepted_handoff_fails_after_confirmed_absence_across_generations() {
    with_isolated_home(|_| {
        let run_id = "generation-confirmed-absence";
        accepted_detached_handoff(run_id);
        let _provider = RunnerContinuationTestGuard::install(Box::new(ReconciliationProvider {
            result: Mutex::new(Some(TestRunnerReconciliation::ConfirmedAbsent(2))),
            recovered_runner_job_id: Mutex::new(None),
        }));

        let terminal = reconcile_status(run_id).expect("terminal lost accepted job");

        assert_eq!(terminal.state, AgentTaskRunState::Failed);
        assert_eq!(terminal.metadata["phase"], "accepted_lab_runner_job_lost");
        assert_eq!(
            terminal.metadata["lost_accepted_runner_job"]["checked_generations"],
            2
        );
        assert_eq!(terminal.metadata["provider_executions_consumed"], 0);
        assert!(terminal.provider_handles.is_empty());
        let aggregate = read_aggregate(run_id).expect("failure aggregate");
        assert!(aggregate.outcomes[0]
            .summary
            .as_deref()
            .is_some_and(|summary| { summary.contains("accepted Lab runner job") }));
    });
}

#[test]
fn accepted_handoff_does_not_terminalize_unconfirmed_generation_absence() {
    with_isolated_home(|_| {
        let run_id = "generation-unconfirmed-absence";
        accepted_detached_handoff(run_id);
        let _provider = RunnerContinuationTestGuard::install(Box::new(ReconciliationProvider {
            result: Mutex::new(Some(TestRunnerReconciliation::Unconfirmed)),
            recovered_runner_job_id: Mutex::new(None),
        }));

        let retained = reconcile_status(run_id).expect("retain unconfirmed handoff");

        assert_eq!(retained.state, AgentTaskRunState::Running);
        assert!(retained.metadata.get("lost_accepted_runner_job").is_none());
        assert_eq!(retained.metadata["provider_executions_consumed"], 0);
        assert!(retained.provider_handles.is_empty());
    });
}

#[test]
fn reserved_lab_admission_recovers_runner_job_after_client_loss() {
    with_isolated_home(|_| {
        let run_id = "recover-reserved-lab-admission";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("persist planned proxy before daemon admission");
        record_lab_admission_reservation_in_store(
            &test_lifecycle_store(),
            run_id,
            "homeboy-lab",
            "daemon-lease-1",
            "reservation-job-1",
            u64::MAX,
        )
        .expect("persist direct daemon reservation before client loss");
        rewrite_record_for_test(run_id, |record| {
            // The synchronous caller has exited and its last controller
            // heartbeat is stale. Recovery must bind the admitted daemon job
            // before the no-PID watchdog classifies this as ownerless.
            record.updated_at = Some(
                (chrono::Utc::now()
                    - chrono::Duration::minutes(
                        homeboy_core::observation::RUNNING_HEARTBEAT_STALE_MINUTES,
                    ))
                .to_rfc3339(),
            );
        })
        .expect("age interrupted caller heartbeat");
        let _provider = RunnerContinuationTestGuard::install(Box::new(ReconciliationProvider {
            result: Mutex::new(Some(TestRunnerReconciliation::Unconfirmed)),
            recovered_runner_job_id: Mutex::new(Some("accepted-runner-job-1".to_string())),
        }));

        let recovered = reconcile_status(run_id).expect("recover admitted runner job");

        assert_eq!(recovered.state, AgentTaskRunState::Running);
        assert_eq!(recovered.runner_id(), Some("homeboy-lab"));
        assert_eq!(recovered.runner_job_id(), Some("accepted-runner-job-1"));
        assert_eq!(
            recovered.metadata["handoff_acceptance"]["state"],
            "accepted"
        );
        assert!(recovered.metadata.get("stale_running").is_none());
        assert!(recovered.metadata.get("stale_running_reason").is_none());
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "Binds before validation" is a claim about one durable row: the
/// pre-acceptance phase record the binder reads its runner from and the accepted
/// handoff it commits back are the same record in the same home. The stub
/// admission keeps submission off the machine-global controller-runtime queue;
/// nothing here asserts on runtime provenance.
///
/// The bind reaches `record_detached_lab_run_in_store`, which carries the
/// default Lab offload submission — but the phase write above has already
/// created the record, so acceptance reads it instead of falling through.
#[test]
fn preacceptance_snapshot_binds_planned_runner_job_before_validation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-preacceptance-snapshot";
    let plan = test_plan();
    let mut record = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "lab_handoff_preacceptance",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist planned controller execution");
    assert!(record.lab_handoff.is_none());
    assert_eq!(record.metadata["runner_id"], "homeboy-lab");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&plan));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
    snapshot.events.clear();

    reconcile_transport_proxy_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("accepted daemon snapshot binds before validation");

    let accepted_job_id = snapshot.job.id.to_string();
    assert_eq!(record.runner_job_id(), Some(accepted_job_id.as_str()));
    assert_eq!(
        record.lab_handoff.as_ref().expect("handoff").state,
        AgentTaskLabHandoffState::Accepted
    );
    assert_eq!(record.metadata["handoff_acceptance"]["state"], "accepted");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Same shape as its sibling above: the pre-acceptance phase record
/// supplies the binding authority and receives the accepted handoff, so both
/// halves have to name one home. The stub admission keeps submission off the
/// machine-global controller-runtime queue.
#[test]
fn preacceptance_snapshot_binds_a_pre_claim_job_without_a_target_runner() {
    // A daemon job is created with `target_runner_id: None` and only gains a
    // runner once claimed, so a snapshot polled before the claim legitimately
    // has no target. The expected-Lab controller handoff is the binding
    // authority: an absent target must still bind (regression for a strict
    // `!=` check that silently skipped this pre-claim window).
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-preacceptance-no-target";
    let plan = test_plan();
    let mut record = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "lab_handoff_preacceptance",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist planned controller execution");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&plan));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.job.target_runner_id = None;
    snapshot.events.clear();

    reconcile_transport_proxy_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("pre-claim daemon snapshot binds before validation");

    let accepted_job_id = snapshot.job.id.to_string();
    assert_eq!(record.runner_job_id(), Some(accepted_job_id.as_str()));
    assert_eq!(
        record.lab_handoff.as_ref().expect("handoff").state,
        AgentTaskLabHandoffState::Accepted
    );
    assert_eq!(record.metadata["handoff_acceptance"]["state"], "accepted");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "Fails closed against the *bound* daemon job" only means anything
/// when the acceptance that bound it and the validation that refuses the
/// mismatch read one record. The run does not exist yet, so the acceptance is
/// spelled with its explicit submission rather than the default one, which would
/// reach the machine-global controller-runtime admission queue — see
/// `stub_lab_offload_submission`.
#[test]
fn preacceptance_snapshot_rejects_a_different_bound_daemon_job() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "cook-preacceptance-mismatch",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("persist accepted controller handoff");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.id =
        uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000456").expect("snapshot job id");
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events.clear();

    let error =
        reconcile_transport_proxy_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
            .expect_err("different accepted daemon job fails closed");

    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert!(error.message.contains("does not match controller job"));
}

#[test]
fn foreground_terminal_projection_binds_a_pending_handoff_before_validation() {
    // Issue #9240: an accepted Lab job can reach an authoritative terminal
    // daemon snapshot before the controller has persisted its accepted runner
    // job id. `project_terminal_runner_result` must bind the still-pending
    // controller handoff to that snapshot's daemon job before validating
    // identity, rather than rejecting a valid terminal snapshot against an empty
    // controller job id.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The pending handoff, the bind the terminal snapshot performs, and
    // the terminal record read back are one home: `project_terminal_runner_result_in_store`
    // decides idempotence by comparing against the aggregate in the store it was
    // handed, so a projection decided against another home's aggregate would
    // either re-project a durable result or skip one.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-terminal-preacceptance-bind";
    let plan = test_plan();
    let record = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "lab_handoff_preacceptance",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist pending controller handoff");
    assert!(
        record.runner_job_id().is_none(),
        "handoff must still be unbound before the terminal snapshot arrives"
    );
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&plan));
    let accepted_job_id = snapshot.job.id.to_string();
    // Point the terminal child lifecycle event at this controller run so the
    // downstream child-identity validation sees the run/job the bind
    // establishes from the same snapshot.
    let identity = &mut snapshot.events[0].data.as_mut().expect("event data")["identity"];
    identity["run_id"] = json!(run_id);
    identity["persisted_run_id"] = json!(run_id);

    let projected = project_terminal_runner_result_in_store(&lifecycle_store, run_id, &snapshot)
        .expect("terminal snapshot binds the pending handoff before validation");
    assert!(
        projected,
        "authoritative terminal snapshot projects the run"
    );

    let bound = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("bound terminal projection")
    .record;
    assert_eq!(bound.runner_job_id(), Some(accepted_job_id.as_str()));
    assert_eq!(bound.state, AgentTaskRunState::Succeeded);
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "The matching aggregate was already projected" is decided by
/// comparing the snapshot against the aggregate this store holds, so the
/// aggregate write, the terminalizing status read, and the projection have to
/// name one home — comparing against another installation's aggregate would
/// skip a projection that was never made here, and would do so without failing.
#[test]
fn terminal_aggregate_binds_runner_job_before_snapshot_validation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-terminal-aggregate-preacceptance-bind";
    let plan = test_plan();
    let record = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "lab_handoff_preacceptance",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("persist planned controller execution");
    let aggregate = succeeded_aggregate(&plan);
    lifecycle_store
        .write_aggregate(run_id, &aggregate)
        .expect("aggregate written");
    let terminal = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("aggregate terminalizes the run")
    .record;
    assert_eq!(terminal.state, AgentTaskRunState::Succeeded);
    assert!(terminal.runner_job_id().is_none());

    let mut snapshot = terminal_child_snapshot(&aggregate);
    let accepted_job_id = snapshot.job.id.to_string();
    let identity = &mut snapshot.events[0].data.as_mut().expect("event data")["identity"];
    identity["run_id"] = json!(run_id);
    identity["persisted_run_id"] = json!(run_id);

    let projected =
        project_terminal_runner_result_in_store(&lifecycle_store, &record.run_id, &snapshot)
            .expect("terminal run binds the daemon job before validation");
    assert!(!projected, "the matching aggregate was already projected");

    let bound = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("bound terminal run")
    .record;
    assert_eq!(bound.state, AgentTaskRunState::Succeeded);
    assert_eq!(bound.runner_id(), Some("homeboy-lab"));
    assert_eq!(bound.runner_job_id(), Some(accepted_job_id.as_str()));
}

#[test]
fn snapshot_validation_reports_missing_controller_identity_distinctly() {
    // Issue #9240: when the controller has never established an accepted runner
    // job identity and no pending handoff can bind one, snapshot validation must
    // surface the missing identity as its own diagnostic instead of comparing a
    // valid runner UUID against an empty string and presenting it as a spurious
    // "does not match controller job " mismatch.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). "There is no controller identity to validate against" is a
    // property of one record in one home, so the submission and the projection
    // that refuses it name the same store. The stub admission keeps submission
    // off the machine-global controller-runtime queue; nothing here asserts on
    // runtime provenance.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-terminal-no-identity";
    let plan = test_plan();
    // A freshly submitted run has no lab handoff and no metadata
    // runner_job_id: there is no controller identity to validate against and
    // no pending handoff to bind one from.
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    assert!(record.lab_handoff.is_none());
    assert!(record.runner_job_id().is_none());

    let snapshot = terminal_child_snapshot(&succeeded_aggregate(&plan));
    let error =
        project_terminal_runner_result_in_store(&lifecycle_store, &record.run_id, &snapshot)
            .expect_err("missing controller identity fails closed with a distinct diagnostic");

    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert!(
        error.message.contains("no accepted runner job identity"),
        "expected a missing-identity diagnostic, got: {}",
        error.message
    );
    assert!(
        !error.message.contains("does not match controller job"),
        "missing identity must not be presented as a runner mismatch: {}",
        error.message
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The plan files this deletes are the ones the injected store owns
/// (`record.plan_path` is that store's controller plan path), and the recovery,
/// the refused handoff, and the terminal record read back all follow it — so
/// "the plan was recovered" and "the handoff failed closed" are properties of
/// one home rather than of whichever plan directory the process environment
/// happened to point at.
#[test]
fn missing_lab_attempt_plan_is_recovered_before_handoff_or_terminalized() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-8096-attempt-1";
    let plan = test_plan();
    let record = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "materializing",
            remote_workspace: None,
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("controller attempt persisted");
    std::fs::remove_file(&record.plan_path).expect("remove interrupted plan");

    let recovered = record_lab_offload_phase_with_submission_in_store(
        &lifecycle_store,
        LabOffloadPhaseRecord {
            requested_run_id: run_id,
            runner_id: "homeboy-lab",
            phase: "dispatching",
            remote_workspace: Some("/runner/workspace/homeboy"),
            source_checkout: None,
            provider_rotation: None,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("controller plan recovery");
    assert_eq!(
        load_plan_in_store(&lifecycle_store, run_id).expect("recovered plan"),
        plan
    );

    std::fs::remove_file(&recovered.plan_path).expect("remove unrecoverable plan");
    let error = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8096",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &[],
        },
    )
    .expect_err("handoff without plan must not become running");
    assert_eq!(error.code, ErrorCode::InternalIoError);

    let terminal = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("terminal recovery record")
    .record;
    assert_eq!(terminal.state, AgentTaskRunState::Failed);
    assert_eq!(
        terminal.metadata["pre_execution_failure"]["phase"],
        "lab_attempt_plan_recovery"
    );
    assert!(terminal.metadata.get("runner_job_id").is_none());
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The whole claim is that a *runner-projected* plan path in the
/// record is display evidence and the controller keeps reading its own durable
/// plan — which is only meaningful when "its own" names one installation. The
/// handoff, the mirrored aggregate, every read back (status, logs, artifacts),
/// the retry that must reuse the durable plan, and the missing-plan fixture all
/// follow `lifecycle_store`.
///
/// The retry is spelled as `retry_with_runtime_admission_in_store(.., false,
/// false, None, ..)`, which is exactly what the ambient `retry` reduces to
/// (`retry_in_store` -> `retry_with_force_inner_in_store` with `force: false,
/// enforce_lineage_reservation: false`), except that its admission is the stub
/// rather than the machine-global controller-runtime one. `test_plan()` carries
/// no Cook candidate evidence, so both workspace restorations inside the retry
/// are no-ops.
#[test]
fn cook_lab_handoff_controller_reads_ignore_runner_plan_projection() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
    ];
    let record = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "cook-lab-attempt",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace",
            remote_command: &command,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("cook handoff persists its controller plan");
    let aggregate = succeeded_aggregate(&plan);
    record_run_aggregate_in_store(&lifecycle_store, &record.run_id, &plan, &aggregate)
        .expect("runner result mirrored to the controller");
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record.plan_path =
            "/home/chubes/.local/share/homeboy/agent-task-runs/cook-lab-attempt/plan.json"
                .to_string();
        record.state = AgentTaskRunState::Running;
    })
    .expect("runner transport projection replaces display path");

    assert_eq!(
        reconcile_status_in_store(
            &lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("controller status")
        .record
        .plan_id,
        plan.plan_id
    );
    assert_eq!(
        logs_in_store(&lifecycle_store, &record.run_id)
            .expect("controller logs")
            .run
            .as_str(),
        record.run_id
    );
    assert_eq!(
        artifacts_in_store(&lifecycle_store, &record.run_id)
            .expect("controller artifacts")
            .run_id,
        record.run_id
    );
    let retry = retry_with_runtime_admission_in_store(
        &lifecycle_store,
        &record.run_id,
        Some("cook-lab-retry"),
        false,
        false,
        None,
        |_| Ok(json!({})),
    )
    .expect("controller retry uses its durable plan");
    assert_eq!(
        load_controller_plan_in_store(&lifecycle_store, &retry.run_id).expect("retry plan"),
        plan
    );

    let missing_plan = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "cook-lab-missing-controller-plan",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace",
            remote_command: &command,
            durable_plan: Some(&plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("missing-plan fixture persists its controller plan");
    rewrite_record_for_test_in_store(&lifecycle_store, &missing_plan.run_id, |record| {
        record.plan_path = "/runner/workspace/plan.json".to_string();
    })
    .expect("project runner-local plan path");
    std::fs::remove_file(missing_plan.plan_path)
        .expect("remove authoritative controller plan despite projected display path");
    let error = reconcile_status_in_store(
        &lifecycle_store,
        &missing_plan.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect_err("missing controller plan fails closed");
    assert_eq!(error.code, ErrorCode::InternalIoError);
}

#[test]
fn runner_terminal_reconciliation_is_idempotent_and_preserves_execution_owner() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-terminal-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-456",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut record = reconcile_status("agent-task-terminal-proxy").expect("status");
        apply_runner_job_terminal_state(
            &mut record,
            homeboy_core::api_jobs::JobStatus::Succeeded,
            &[],
        );
        store::write_record(&record).expect("terminal record");
        let receipt = resolve_workspace_terminal_authority(
            "agent-task-terminal-proxy",
            "homeboy-lab",
            "/runner/workspace/repo",
            Some("job-456"),
        )
        .expect("resolve terminal workspace authority")
        .expect("terminal workspace authority persisted");
        assert_eq!(receipt.runner_job_id, "job-456");

        let retry = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-terminal-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-456",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("same child handoff is idempotent");
        assert_eq!(retry.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            retry.metadata["runner_execution_record"]["status"],
            "succeeded"
        );
        assert_eq!(
            retry.metadata["runner_execution_record"]["job_id"],
            "job-456"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The acceptance is written into, and the reconciliation commits back
/// into, one store, so the liveness the snapshot clears is the liveness this
/// home recorded. The run does not exist yet, so the acceptance is spelled with
/// its explicit submission rather than the default one, which would reach the
/// machine-global controller-runtime admission queue — see
/// `stub_lab_offload_submission`.
#[test]
fn reachable_running_child_clears_disconnected_liveness_and_refreshes_heartbeat() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-reconnected-running",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    record.annotate_runner_disconnected();
    let disconnected_heartbeat = record.lifecycle.heartbeat.clone();

    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events.clear();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("reachable reconciliation");

    assert_eq!(record.state, AgentTaskRunState::Running);
    assert_eq!(record.metadata["runner_liveness"], "reachable");
    assert!(record.metadata.get("stale_running").is_none());
    assert!(record.metadata.get("stale_running_reason").is_none());
    assert!(record.metadata.get("retryable").is_none());
    assert_ne!(record.lifecycle.heartbeat, disconnected_heartbeat);
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The live progress the reconciliation commits and the log projection
/// read back out are the same home, so "the provider handle and its log event
/// are durable" is asserted about the record the snapshot was applied to.
#[test]
fn running_child_snapshot_persists_provider_handle_and_live_log_progress() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-live-provider",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events = vec![homeboy_core::api_jobs::JobEvent {
        sequence: 1,
        job_id: snapshot.job.id,
        kind: homeboy_core::api_jobs::JobEventKind::Progress,
        timestamp_ms: 2,
        message: Some("provider dispatch accepted".to_string()),
        data: Some(json!({
            "metadata": {
                "provider_handle": AgentTaskExecutionHandle {
                    kind: crate::agent_task::AgentTaskExecutionHandleKind::ProviderRun,
                    task_id: "task-a".to_string(),
                    backend: "openai/gpt-5.6-terra".to_string(),
                    run_id: "provider-run-live".to_string(),
                    stream_uri: Some("provider://runs/provider-run-live/events".to_string()),
                    metadata: json!({"progress": "accepted"}),
                }
            }
        })),
    }];

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("live reconciliation");

    assert_eq!(record.metadata["phase"], "executing");
    assert_eq!(record.metadata["provider_state"], "active");
    assert_eq!(record.provider_handles.len(), 1);
    assert_eq!(
        record.provider_handles[0].provider_run_id,
        "provider-run-live"
    );
    let log = logs_in_store(&lifecycle_store, &record.run_id).expect("live logs");
    assert_eq!(log.events.len(), 1);
    assert!(log.events[0].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider dispatch accepted")));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "The stale running writer is ignored" is a claim about one durable
/// row: the write that must lose, the terminal state that must win, and the
/// status read that adjudicates between them all name `lifecycle_store`.
#[test]
fn terminal_runner_reconciliation_never_resurrects_a_controller_record() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let before = record.clone();
    let terminal = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &terminal)
        .expect("terminal reconciliation");
    let terminal_record = record.clone();

    lifecycle_store
        .write_record(&before)
        .expect("stale running writer is ignored");
    assert_eq!(
        reconcile_status_in_store(
            &lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("terminal state remains committed")
        .record
        .state,
        AgentTaskRunState::Succeeded
    );

    let mut running = terminal.clone();
    running.job.status = homeboy_core::api_jobs::JobStatus::Running;
    running.events.clear();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &running)
        .expect("terminal records stay immutable");

    assert_eq!(record, terminal_record);
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Only the acceptance is durable here — `annotate_runner_disconnected`
/// is an in-memory record mutation — so rooting the acceptance is the whole
/// isolation this test needs.
#[test]
fn disconnected_runner_marks_nonterminal_proxy_stale_without_advancing_heartbeat() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-disconnected-running",
            runner_id: "homeboy-lab",
            runner_job_id: "job-789",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let heartbeat = record.lifecycle.heartbeat.clone();

    record.annotate_runner_disconnected();

    assert_eq!(record.state, AgentTaskRunState::Running);
    assert_eq!(record.lifecycle.heartbeat, heartbeat);
    assert_eq!(record.metadata["runner_liveness"], "disconnected");
    assert_eq!(record.metadata["stale_running"], true);
    assert_eq!(
        record.metadata["stale_running_reason"],
        "runner_disconnected"
    );
}

#[test]
fn detached_runner_failure_transitions_parent_and_task_terminal() {
    let plan = test_plan();
    let mut record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id: "detached-run".to_string(),
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Running,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: "plan.json".to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle: lifecycle_for_submitted_plan(&plan),
        lab_handoff: None,
        candidate_adoption: None,
        adoption_run_id: None,
        acceptance: None,
        workspace_identity: None,
        workspace_lifecycle_revision: 0,
        workspace_owner_lease: None,
        workspace_claim: None,
        metadata: json!({ "runner_id": "homeboy-lab", "runner_job_id": "job-123" }),
    };
    record.tasks[0].state = AgentTaskState::Running;

    apply_runner_job_terminal_state(&mut record, homeboy_core::api_jobs::JobStatus::Failed, &[]);

    assert_eq!(record.state, AgentTaskRunState::Failed);
    assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
    assert_eq!(record.lifecycle.execution.state, RunExecutionState::Failed);
    assert_eq!(record.metadata["runner_job_status"], "failed");
    assert_eq!(record.metadata["retryable"], true);
}

#[test]
fn terminal_reconciliation_rejects_conflicting_directly_imported_artifact() {
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The conflicting artifact is imported into, and the terminal
    // projection is read back out of, this store's own observation database and
    // artifact root — so the refusal asserted below is a property of one home.
    // `test_plan()` carries no workspace root, so `record_aggregate_in_store`
    // (reached through `record_run_aggregate_in_store`) skips the still-ambient
    // automatic artifact-retention pass.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let patch = b"patch bytes";
    let conflicting = b"other bytes";
    let source = context.root().join("conflicting.patch");
    std::fs::write(&source, conflicting).expect("write conflicting patch");
    let plan = test_plan();
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "patch".to_string(),
        kind: "patch".to_string(),
        name: None,
        label: None,
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some("/runner/private/patch.diff".to_string()),
        url: None,
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
        metadata: json!({ "executor_artifact_finalized": true }),
    });
    let submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "direct-import-conflict", |_| Ok(json!({})))
        .expect("submit");
    record_runner_job_identity_in_store(
        &lifecycle_store,
        &submitted.run_id,
        "homeboy-lab",
        "job-1",
    )
    .expect("runner identity");

    let mut hash = sha2::Sha256::new();
    sha2::Digest::update(&mut hash, submitted.run_id.as_bytes());
    sha2::Digest::update(&mut hash, [0]);
    sha2::Digest::update(&mut hash, aggregate.outcomes[0].task_id.as_bytes());
    sha2::Digest::update(&mut hash, [0]);
    sha2::Digest::update(&mut hash, b"patch");
    let artifact_id = format!("agent-task-{:x}", hash.finalize());
    let store = lifecycle_store
        .open_observation_initialized()
        .expect("store");
    store
        .import_artifact(&homeboy_core::observation::ArtifactRecord {
            id: artifact_id,
            run_id: submitted.run_id.clone(),
            kind: "patch".to_string(),
            artifact_type: "file".to_string(),
            path: source.display().to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: Some(format!("{:x}", sha2::Sha256::digest(conflicting))),
            size_bytes: Some(conflicting.len() as i64),
            mime: Some("text/x-patch".to_string()),
            metadata_json: json!({ "name": "patch" }),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .expect("conflicting direct artifact import");

    record_run_aggregate_in_store(&lifecycle_store, &submitted.run_id, &plan, &aggregate)
        .expect("terminal state is persisted");
    let record = lifecycle_store
        .read_record(&submitted.run_id)
        .expect("terminal record");
    assert_eq!(record.metadata["artifact_projection"]["status"], "failed");
    assert!(record.metadata["artifact_projection"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("conflicts with terminal artifact projection")));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The Cook index, the two attempts it names, and the alias resolution
/// that has to land on the second one are one home — an index read in one
/// installation while the attempts live in another would resolve a "latest" that
/// this store never recorded.
///
/// `record_completed_run` is spelled out as its own two rooted halves —
/// submission then `record_aggregate_in_store` — which is exactly the body of
/// `record_completed_run_in_store`. The difference is the admission: that
/// sibling submits through `submit_plan_in_store`, which resolves the
/// controller-runtime admission queue under `paths::controller_runtimes_store()`.
/// That store is machine-global by design, so calling it from a test that no
/// longer mutates HOME would enqueue against the real operator runtime store.
/// `test_plan()` carries no workspace root, so `record_aggregate_in_store` skips
/// the still-ambient automatic artifact-retention pass.
#[test]
fn cook_index_keeps_repeated_attempts_unique_with_stable_latest_alias() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let aggregate = succeeded_aggregate(&plan);
    let first_run_id = cook_attempt_run_id("cook-issue-6978", 1);
    let second_run_id = cook_attempt_run_id("cook-issue-6978", 1);

    assert_ne!(first_run_id, second_run_id);

    let mut first = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, &first_run_id, |_| Ok(json!({})))
        .expect("first run recorded");
    record_aggregate_in_store(&lifecycle_store, &mut first, &plan, &aggregate)
        .expect("first run recorded");
    record_cook_attempt_in_store(&lifecycle_store, "cook-issue-6978", 1, &first_run_id)
        .expect("first cook indexed");
    let mut second = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, &second_run_id, |_| Ok(json!({})))
        .expect("second run recorded");
    record_aggregate_in_store(&lifecycle_store, &mut second, &plan, &aggregate)
        .expect("second run recorded");
    record_cook_attempt_in_store(&lifecycle_store, "cook-issue-6978", 1, &second_run_id)
        .expect("second cook indexed");

    let index =
        cook_index_in_store(&lifecycle_store, "cook-issue-6978").expect("cook index loaded");
    assert_eq!(index.latest_run_id, second_run_id);
    assert_eq!(index.attempts.len(), 2);
    assert_eq!(index.attempts[0].run_id, first_run_id);
    assert_eq!(index.attempts[1].run_id, second_run_id);

    let latest = reconcile_status_in_store(
        &lifecycle_store,
        "cook-issue-6978",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("stable cook id resolves")
    .record;
    assert_eq!(latest.run_id, second_run_id);
    assert_eq!(latest.metadata["cook_alias"], "cook-issue-6978");
    assert_eq!(
        latest.metadata["cook_index"]["latest_run_id"],
        second_run_id
    );

    let (_raw, path) = aggregate_source_in_store(&lifecycle_store, "cook-issue-6978")
        .expect("latest aggregate resolves");
    assert!(path.display().to_string().contains(&second_run_id));
}

#[test]
fn run_record_exists_resolves_a_cook_id_to_its_latest_run() {
    // #8390: the Lab retry handoff guarded on the exact-match `run_record_exists`,
    // so a resolvable id (e.g. a cook id) reported absent even though `retry`
    // would succeed, and the handoff silently fell through to ship an unrunnable
    // `agent-task retry <id>` to the runner. `run_record_exists_resolved` must
    // report present for a cook id that resolves to a real run.
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). `record_completed_run` is spelled out as its own two rooted
    // halves — submission then `record_aggregate_in_store` — which is exactly
    // the body of `record_completed_run_in_store`. The difference is the
    // admission: that sibling submits through `submit_plan_in_store`, which
    // resolves the controller-runtime admission queue under
    // `paths::controller_runtimes_store()`. That store is machine-global by
    // design, so calling it from a test that no longer mutates HOME would
    // enqueue against the real operator runtime store. `test_plan()` carries no
    // workspace root, so `record_aggregate_in_store` skips the still-ambient
    // automatic artifact-retention pass.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let aggregate = succeeded_aggregate(&plan);
    let run_id = cook_attempt_run_id("cook-issue-8390", 1);
    let mut submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, &run_id, |_| Ok(json!({})))
        .expect("run recorded");
    record_aggregate_in_store(&lifecycle_store, &mut submitted, &plan, &aggregate)
        .expect("run recorded");
    record_cook_attempt_in_store(&lifecycle_store, "cook-issue-8390", 1, &run_id)
        .expect("cook indexed");

    // Exact match sees only the concrete run id, not the cook alias.
    assert!(run_record_exists_in_store(&lifecycle_store, &run_id).expect("exact run exists"));
    assert!(
        !run_record_exists_in_store(&lifecycle_store, "cook-issue-8390")
            .expect("cook id not an exact record")
    );

    // Resolution-aware existence follows the same path `retry` uses.
    assert!(
        run_record_exists_resolved_in_store(&lifecycle_store, &run_id)
            .expect("resolved run exists")
    );
    assert!(
        run_record_exists_resolved_in_store(&lifecycle_store, "cook-issue-8390")
            .expect("cook id resolves"),
        "a cook id must resolve to its latest run for the Lab retry handoff"
    );
    assert!(
        !run_record_exists_resolved_in_store(&lifecycle_store, "cook-does-not-exist")
            .expect("missing id"),
        "a genuinely missing id must still report absent"
    );
}

#[test]
fn remote_dispatch_failure_preserves_structured_outcome_details() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "logs".to_string(),
                    uri: "homeboy://agent-task/run/remote-run/logs".to_string(),
                    label: Some("remote provider logs".to_string()),
                }],
                outputs: serde_json::json!({
                    "provider_run_result": {
                        "status": "failed",
                        "failure_classification": "runtime",
                        "artifacts": [],
                        "refs": { "logs": [], "transcripts": [], "runtimes": [] }
                    }
                }),
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "remote_run_id": "provider-run-1",
                    "remote_workspace": "/runner/workspace/repo"
                }),
                ..Default::default()
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some("Remote provider agent task failed.".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let remote_record =
            record_completed_run(&plan, &aggregate, Some("remote-run")).expect("remote record");
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": plan.plan_id,
            "state": "failed",
            "record": remote_record,
            "aggregate": aggregate,
        });

        let record = record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "local-run",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                stdout: &envelope.to_string(),
                stderr: "",
                exit_code: 1,
            },
            &envelope,
        )
        .expect("remote dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = reconcile_status("local-run").expect("status loaded");
        let log = logs("local-run").expect("logs loaded");
        let artifacts = artifacts("local-run").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("local-run").expect("aggregate source");

        assert_eq!(record.run_id, "local-run");
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].task_id, "task-a");
        assert_ne!(loaded.tasks[0].task_id, "agent-task-predispatch");
        assert_eq!(
            loaded.metadata["kind"],
            "lab_offload_remote_dispatch_failure"
        );
        assert_eq!(loaded.metadata["runner_id"], "lab-a");
        assert!(std::path::Path::new(&loaded.plan_path).is_file());
        let loaded_plan = load_plan("local-run").expect("plan loaded");
        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(loaded_plan.tasks[0].task_id, "task-a");
        assert_eq!(
            loaded.metadata["remote_workspace"],
            "/runner/workspace/repo"
        );
        assert_eq!(
            log.events[0].data["message"].as_str(),
            Some("Remote provider agent task failed.")
        );
        assert_eq!(artifacts.evidence_refs[0].kind, "logs");
        assert!(raw_aggregate.contains("fixture.agent-task-executor"));
        assert!(raw_aggregate.contains("failure_classification"));
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The completed run is written and read back through one injected
/// store, so the executor evidence and the artifact report asserted below are
/// projections of the same home.
///
/// `record_completed_run` is spelled out as its own two rooted halves —
/// submission then `record_aggregate_in_store` — which is exactly the body of
/// `record_completed_run_in_store`. The difference is the admission: that
/// sibling submits through `submit_plan_in_store`, which resolves the
/// controller-runtime admission queue under `paths::controller_runtimes_store()`.
/// That store is machine-global by design, so calling it from a test that no
/// longer mutates HOME would enqueue against the real operator runtime store and
/// block on its cross-process lock. The stub admission is the same one every
/// other rooted test in this crate passes.
///
/// The plan carries no workspace root, so `record_aggregate_in_store` skips the
/// automatic artifact-retention pass — which is still ambient, and is why the
/// sibling test that asserts on `automatic_artifact_retention` was left on
/// `with_isolated_home`.
#[test]
fn completed_run_exposes_latest_executor_input_output_and_expectations() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let mut plan = test_plan();
    let request = &mut plan.tasks[0];
    request.executor.backend = "sandbox".to_string();
    request.executor.model = Some("gpt-fixture".to_string());
    request.component_contracts = vec![AgentTaskComponentContract {
        slug: Some("runtime-engine".to_string()),
        path: Some("/workspace/runtime-engine".to_string()),
        extra: serde_json::Map::from_iter([
            ("loadAs".to_string(), json!("plugin")),
            ("activate".to_string(), json!(true)),
        ]),
    }];
    request.metadata = json!({
        "runtime_component_paths": ["/runtime/components/sandbox-host"]
    });
    request.expected_artifacts = vec!["patch".to_string()];
    request.artifact_declarations = vec![AgentTaskArtifactDeclaration {
        name: "proof_bundle".to_string(),
        artifact_type: Some("bundle".to_string()),
        artifact_schema: None,
        path: None,
        required: true,
        description: None,
        metadata: Value::Null,
    }];

    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].metadata = json!({
        "model": "openai/gpt-5.6-terra",
        "provider_rotation": {
            "attempts": [{
                "attempt": 1,
                "rotation_index": 0,
                "backend": "sandbox",
                "selector": "fixture",
                "model": "gpt-fixture",
                "requested_model": "gpt-fixture",
                "attempted_model": "gpt-fixture",
                "candidate_producing_model": "gpt-fixture",
                "status": "provider_error",
                "failure_classification": "provider"
            }, {
                "attempt": 2,
                "rotation_index": 1,
                "backend": "opencode",
                "selector": "opencode.agent-task-executor",
                "model": "openai/gpt-5.6-terra",
                "requested_model": "gpt-fixture",
                "attempted_model": "openai/gpt-5.6-terra",
                "candidate_producing_model": "openai/gpt-5.6-terra",
                "status": "succeeded"
            }]
        }
    });
    aggregate.outcomes[0].outputs = json!({
        "provider_run_result": {
            "run_id": "provider-run-123",
            "status": "succeeded"
        }
    });

    let mut submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-evidence", |_| Ok(json!({})))
        .expect("submitted");
    let record = record_aggregate_in_store(&lifecycle_store, &mut submitted, &plan, &aggregate)
        .expect("recorded");
    let evidence = record
        .latest_executor_evidence
        .as_ref()
        .expect("latest executor evidence");
    let artifact_report =
        artifacts_in_store(&lifecycle_store, "run-evidence").expect("artifacts loaded");

    assert_eq!(evidence.task_id, "task-a");
    assert_eq!(evidence.backend, "opencode");
    assert_eq!(
        evidence.selector.as_deref(),
        Some("opencode.agent-task-executor")
    );
    assert_eq!(evidence.model.as_deref(), Some("openai/gpt-5.6-terra"));
    assert_eq!(record.tasks[0].backend, "opencode");
    assert_eq!(
        record.tasks[0].model.as_deref(),
        Some("openai/gpt-5.6-terra")
    );
    assert_eq!(
        evidence.provider_run_id.as_deref(),
        Some("provider-run-123")
    );
    assert_eq!(evidence.component_contracts.len(), 1);
    assert_eq!(
        evidence.runtime_component_paths,
        vec![
            "/runtime/components/sandbox-host".to_string(),
            "/workspace/runtime-engine".to_string()
        ]
    );
    assert_eq!(evidence.expected_artifacts, vec!["patch".to_string()]);
    assert_eq!(
        evidence.typed_artifact_expectations,
        vec!["proof_bundle".to_string()]
    );
    assert_eq!(
        record.metadata["latest_executor_evidence"]["input_ref"]["uri"],
        "homeboy://agent-task/run/run-evidence/plan#task=task-a"
    );
    assert!(artifact_report
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "executor-input"));
    assert!(artifact_report
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "executor-normalized-output"));
    assert!(artifact_report
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "executor-outcome"));
}

#[test]
fn run_state_bridges_one_to_one_onto_execution_state() {
    let cases = [
        (AgentTaskRunState::Queued, RunExecutionState::Queued),
        (AgentTaskRunState::Running, RunExecutionState::Running),
        (AgentTaskRunState::Succeeded, RunExecutionState::Succeeded),
        // These two were the exception the test name claimed did not exist:
        // both mapped to `PartialFailure` until #6761, so the bridge was 8->6,
        // not one-to-one. Listed explicitly now that they carry through.
        (
            AgentTaskRunState::CandidateRecoverable,
            RunExecutionState::CandidateRecoverable,
        ),
        (
            AgentTaskRunState::PartialRecoverable,
            RunExecutionState::PartialRecoverable,
        ),
        (
            AgentTaskRunState::PartialFailure,
            RunExecutionState::PartialFailure,
        ),
        (AgentTaskRunState::Failed, RunExecutionState::Failed),
        (AgentTaskRunState::Cancelled, RunExecutionState::Cancelled),
    ];
    for (run_state, expected) in cases {
        assert_eq!(RunExecutionState::from(run_state), expected);
    }
}

#[test]
fn failed_provider_run_exposes_workflow_evidence_refs() {
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). See `run_record_exists_resolves_a_cook_id_to_its_latest_run` for
    // why `record_completed_run` is spelled out as its two rooted halves.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            queued: 1,
            failed: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            task_id: "task-a".to_string(),
            status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
            summary: Some("provider task failed".to_string()),
            failure_classification: Some(
                crate::agent_task::AgentTaskFailureClassification::ExecutionFailed,
            ),
            workflow: Some(AgentTaskWorkflowEvidence {
                schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
                id: "provider-run-123".to_string(),
                label: Some("provider workflow".to_string()),
                steps: vec![AgentTaskWorkflowStepEvidence {
                    id: "runtime".to_string(),
                    label: Some("runtime evidence".to_string()),
                    status: AgentTaskWorkflowStepStatus::Failed,
                    depends_on: Vec::new(),
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    metrics: Value::Null,
                    artifact_refs: vec![AgentTaskEvidenceRef {
                        kind: "provider-transcript".to_string(),
                        uri: "provider://runs/provider-run-123/transcript".to_string(),
                        label: Some("Provider transcript".to_string()),
                    }],
                    diagnostics: Vec::new(),
                    suggestions: Vec::new(),
                    metadata: Value::Null,
                }],
                metadata: Value::Null,
            }),
            ..Default::default()
        }],
        events: vec![AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Failed,
            attempt: 1,
            message: Some("provider task failed".to_string()),
        }],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    };

    let mut submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-provider-failed", |_| Ok(json!({})))
        .expect("recorded");
    let record = record_aggregate_in_store(&lifecycle_store, &mut submitted, &plan, &aggregate)
        .expect("recorded");
    let durable_status = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status")
    .record;
    let durable_artifacts =
        artifacts_in_store(&lifecycle_store, &record.run_id).expect("artifacts");

    assert_eq!(durable_status.state, AgentTaskRunState::Failed);
    assert_eq!(durable_status.artifact_refs.len(), 1);
    assert_eq!(durable_status.artifact_refs[0].kind, "provider-transcript");
    assert_eq!(durable_artifacts.evidence_refs.len(), 4);
    assert_eq!(
        durable_artifacts.evidence_refs[0].uri,
        "provider://runs/provider-run-123/transcript"
    );
    assert!(durable_artifacts
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "executor-input"));
}

#[test]
fn status_marks_running_run_without_owner_as_stale() {
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The stub admission keeps submission off the machine-global
    // controller-runtime queue; `reconcile_status_in_store` reads its admission from this
    // store's own controller-runtime root, so the staleness classification and
    // the record it is persisted onto are projections of the same home.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-stale-missing-owner", |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store
        .read_record("run-stale-missing-owner")
        .expect("record");
    record.state = AgentTaskRunState::Running;
    lifecycle_store
        .write_record(&record)
        .expect("stored running record");

    let loaded = reconcile_status_in_store(
        &lifecycle_store,
        "run-stale-missing-owner",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status loaded")
    .record;

    assert_eq!(loaded.state, AgentTaskRunState::Running);
    assert_eq!(loaded.metadata["stale_running"], json!(true));
    assert_eq!(
        loaded.metadata["stale_running_reason"],
        "missing_runner_pid"
    );
    assert_eq!(loaded.metadata["provider_boundary"]["status"], "absent");

    // Read-side reconciliation persists the classification, so repeated
    // status reads converge instead of reviving a ghost run as active.
    let persisted = lifecycle_store
        .read_record("run-stale-missing-owner")
        .expect("persisted record");
    assert_eq!(persisted.metadata["stale_running"], json!(true));
    let repeated = reconcile_status_in_store(
        &lifecycle_store,
        "run-stale-missing-owner",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("repeated status loaded")
    .record;
    assert_eq!(repeated.metadata["stale_running"], json!(true));
}

#[test]
fn status_keeps_fresh_planned_runner_submission_live() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let run_id = "run-planned-runner-submission";
        submit_plan(&plan, Some(run_id)).expect("submitted");
        mark_running(run_id).expect("running");
        rewrite_record_for_test(run_id, |record| {
            record
                .metadata
                .as_object_mut()
                .expect("metadata object")
                .remove("runner_pid");
        })
        .expect("controller owner removed");
        let stale = reconcile_status(run_id).expect("pre-planning status loaded");
        assert_eq!(stale.metadata["stale_running"], true);

        record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("planned runner submission recorded");

        let loaded = reconcile_status(run_id).expect("status loaded");

        assert_eq!(loaded.state, AgentTaskRunState::Running);
        assert_eq!(
            loaded.metadata["runner_execution_record"]["status"],
            "planned"
        );
        assert!(loaded.metadata.get("stale_running").is_none());

        rewrite_record_for_test(run_id, |record| {
            record.updated_at = Some(
                (chrono::Utc::now()
                    - chrono::Duration::minutes(
                        homeboy_core::observation::RUNNING_HEARTBEAT_STALE_MINUTES,
                    ))
                .to_rfc3339(),
            );
        })
        .expect("planned submission aged past heartbeat threshold");
        let stale = reconcile_status(run_id).expect("stale status loaded");
        assert_eq!(stale.metadata["stale_running"], true);
        assert_eq!(stale.metadata["stale_running_reason"], "missing_runner_pid");
    });
}

#[test]
fn planned_lab_proxy_stamps_heartbeat_without_a_runner_pid() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let run_id = "run-planned-lab-proxy-heartbeat";
        submit_plan(&plan, Some(run_id)).expect("submitted");
        record_cook_progress_in_store(&test_lifecycle_store(), run_id, "provider_start", 1, None)
            .expect("provider_start");
        let remote_command = ["homeboy".to_string(), "agent-task".to_string()];
        let recorded = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &remote_command,
            durable_plan: Some(&plan),
        })
        .expect("planned Lab proxy");
        rewrite_record_for_test(run_id, |record| {
            record
                .metadata
                .as_object_mut()
                .expect("metadata object")
                .remove("runner_pid");
        })
        .expect("initiating client ended");

        assert!(recorded.updated_at.is_some(), "{recorded:?}");
        assert!(recorded.has_planned_runner_execution(), "{recorded:?}");
        assert!(recorded.is_controller_pre_provider_phase(), "{recorded:?}");

        let loaded = reconcile_status(run_id).expect("status loaded");
        assert!(loaded.has_fresh_update(), "{loaded:?}");
        assert!(loaded.has_fresh_controller_pre_provider_heartbeat());
        assert!(loaded.metadata.get("stale_running").is_none());
        assert_eq!(
            loaded.metadata["runner_execution_record"]["status"],
            "planned"
        );
        assert_eq!(loaded.metadata["cook_progress"]["phase"], "provider_start");
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Cancellation resolves the alias, mutates the record and then reads
/// it back; all three name `lifecycle_store`, so the durable cancellation
/// asserted below is the one this test committed rather than a state some other
/// home happened to hold. The stub admission keeps submission off the
/// machine-global controller-runtime queue.
///
/// The daemon cancellation hook stays where it is: `test_cancel_hook` is a
/// `thread_local!`, not a process-global, so it was never protected by the
/// hermetic-home mutex and is unaffected by dropping it.
#[test]
fn cancel_run_marks_queued_record_cancelled() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-cancel-queued", |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store
        .read_record("run-cancel-queued")
        .expect("record");
    record.metadata = json!({
        "runner_id": "homeboy-lab",
        "runner_job_id": "queued-reservation",
    });
    lifecycle_store
        .write_record(&record)
        .expect("store runner reservation");
    let _cancel = super::cancellation::test_cancel_hook::install(Box::new(
        |runner_id, job_id, _durable_run_id| {
            assert_eq!(runner_id, "homeboy-lab");
            assert_eq!(job_id, "queued-reservation");
            Ok((
                homeboy_core::api_jobs::Job {
                    id: uuid::Uuid::new_v4(),
                    operation: "runner.exec".to_string(),
                    status: homeboy_core::api_jobs::JobStatus::Cancelled,
                    created_at_ms: 1,
                    updated_at_ms: 2,
                    started_at_ms: None,
                    finished_at_ms: Some(2),
                    event_count: 0,
                    source_snapshot: None,
                    path_materialization_plan: None,
                    stale_reason: None,
                    daemon_lease_id: None,
                    target_runner_id: None,
                    target_project_id: None,
                    claim_id: None,
                    claimed_by_runner_id: None,
                    claimed_at_ms: None,
                    claim_expires_at_ms: None,
                    artifacts: Vec::new(),
                    runner_job_projection: None,
                },
                Vec::new(),
            ))
        },
    ));

    let cancelled = cancel_run_in_store(&lifecycle_store, "run-cancel-queued", Some("loser cell"))
        .expect("queued run cancelled");
    let loaded = reconcile_status_in_store(
        &lifecycle_store,
        "run-cancel-queued",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status loaded")
    .record;

    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
    assert_eq!(cancelled.metadata["cancel_reason"], json!("loser cell"));
    assert_eq!(
        cancelled.metadata["live_cancellation"]["cancellation"],
        "runner_job_cancel"
    );
    assert_eq!(loaded.state, AgentTaskRunState::Cancelled);
}

#[test]
fn list_records_skips_malformed_observation_records() {
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The malformed row is inserted through this store's own
    // observation database — the same one `list_records_in_store` scans — so the
    // skip asserted below is a property of one home rather than of whichever
    // observation DB the process environment happened to point at.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "good-run", |_| Ok(json!({})))
        .expect("submitted");
    let observation_store = lifecycle_store
        .open_observation_initialized()
        .expect("observation store");
    observation_store
        .upsert_imported_run(&homeboy_core::observation::RunRecord {
            id: "bad-run".to_string(),
            kind: "agent-task".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            status: "running".to_string(),
            metadata_json: json!({ "schema": "homeboy/agent-task-observation-record/v1" }),
            ..Default::default()
        })
        .expect("bad record inserted");
    store::fail_next_record_write_for_test();

    let records = list_records_in_store(&lifecycle_store).expect("records listed");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, "good-run");
    assert!(
        lifecycle_store.write_record(&records[0]).is_err(),
        "listing must leave the injected write failure unconsumed"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The malformed rows are inserted through this store's own
/// observation database — the same one `record_health_summary_in_store` counts
/// — so `health.malformed == malformed_count` is an exact equality about one
/// home. Counted ambiently it would be an equality against whichever
/// observation DB the process environment pointed at, which on a developer or
/// operator machine holds unrelated malformed rows and would make the assertion
/// either fail or pass for the wrong reason.
#[test]
fn record_health_summary_stays_bounded_with_many_malformed_records() {
    // A state directory full of historical malformed agent-task records must
    // not produce unbounded per-record output. The health summary aggregates
    // every malformed record into a total count while capping the retained
    // samples, so read-only activity/upgrade output stays bounded. (#8397)
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let malformed_count = crate::agent_task_lifecycle::health::HEALTH_SAMPLE_LIMIT * 3;
    let store = lifecycle_store
        .open_observation_initialized()
        .expect("observation store");
    for index in 0..malformed_count {
        store
            .upsert_imported_run(&homeboy_core::observation::RunRecord {
                id: format!("bad-run-{index}"),
                kind: "agent-task".to_string(),
                component_id: None,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                finished_at: None,
                status: "running".to_string(),
                command: None,
                cwd: None,
                homeboy_version: None,
                git_sha: None,
                rig_id: None,
                // A record with the observation schema but no `agent_task_run`
                // metadata is classified as MissingMetadata (malformed).
                metadata_json: json!({
                    "schema": "homeboy/agent-task-observation-record/v1"
                }),
            })
            .expect("malformed record inserted");
    }

    let health = record_health_summary_in_store(&lifecycle_store).expect("health summary");

    // Every malformed record is counted…
    assert_eq!(health.malformed, malformed_count);
    // …but the retained sample set stays bounded regardless of volume.
    assert!(
        health.samples.len() <= crate::agent_task_lifecycle::health::HEALTH_SAMPLE_LIMIT,
        "samples ({}) must not exceed HEALTH_SAMPLE_LIMIT ({})",
        health.samples.len(),
        crate::agent_task_lifecycle::health::HEALTH_SAMPLE_LIMIT
    );
    // Each retained sample carries an actionable remediation command.
    for sample in &health.samples {
        assert!(
            !sample.remediation.is_empty(),
            "each malformed sample must carry a remediation hint"
        );
    }
}

#[test]
fn artifact_refs_omit_evidence_refs_with_empty_uri() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        Vec::new(),
        vec![
            AgentTaskEvidenceRef {
                kind: "sample-runtime-command-log".to_string(),
                uri: "".to_string(),
                label: Some("command log".to_string()),
            },
            AgentTaskEvidenceRef {
                kind: "sample-runtime-command-evidence".to_string(),
                uri: "   ".to_string(),
                label: None,
            },
            AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: "file:///tmp/transcript.json".to_string(),
                label: Some("provider transcript".to_string()),
            },
        ],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1, "empty/whitespace evidence URIs are dropped");
    assert_eq!(refs[0].kind, "transcript");
    assert_eq!(refs[0].uri, "file:///tmp/transcript.json");
}

#[test]
fn status_filters_empty_uri_artifact_refs() {
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). See `run_record_exists_resolves_a_cook_id_to_its_latest_run` for
    // why `record_completed_run` is spelled out as its two rooted halves.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Succeeded,
        totals: AgentTaskAggregateTotals {
            queued: 1,
            succeeded: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![outcome_with_refs(
            "task-a",
            vec![
                artifact_ref_artifact(
                    "dir-empty",
                    "sample-runtime-artifact-directory",
                    Some(""),
                    None,
                ),
                artifact_ref_artifact("patch", "patch", None, Some("/tmp/patch.diff")),
            ],
            vec![
                AgentTaskEvidenceRef {
                    kind: "sample-runtime-command-log".to_string(),
                    uri: "".to_string(),
                    label: Some("command log".to_string()),
                },
                AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "file:///tmp/transcript.json".to_string(),
                    label: Some("provider transcript".to_string()),
                },
            ],
        )],
        events: vec![AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Succeeded,
            attempt: 1,
            message: Some("ok".to_string()),
        }],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    };

    let mut submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-empty-refs", |_| Ok(json!({})))
        .expect("recorded");
    let record = record_aggregate_in_store(&lifecycle_store, &mut submitted, &plan, &aggregate)
        .expect("recorded");
    let durable_status = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status")
    .record;

    let uris: Vec<&str> = durable_status
        .artifact_refs
        .iter()
        .map(|r| r.uri.as_str())
        .collect();
    assert!(
        uris.iter().all(|uri| !uri.is_empty()),
        "no empty-URI refs leak into status output: {uris:?}"
    );
    let kinds: Vec<&str> = durable_status
        .artifact_refs
        .iter()
        .map(|r| r.kind.as_str())
        .collect();
    assert_eq!(kinds, vec!["patch", "transcript"]);
}
