//! Tests for agent_task_lifecycle (extracted from mod.rs to keep mod.rs under structural thresholds).
#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskExecutionHandle, AgentTaskExecutor,
    AgentTaskLimits, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence, AgentTaskWorkflowStepStatus,
    AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use crate::api_jobs::{JobEvent, JobEventKind};
use crate::test_support::with_isolated_home;
use sha2::{Digest, Sha256};

#[cfg(unix)]
fn fake_controller_artifact(path: &std::path::Path, identity: &str, marker: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let identity = serde_json::to_string(identity).expect("serialize fake controller identity");
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n# {marker}\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = self ] && [ \"$2\" = status ]; then\n  printf '%s\\n' '{{\"data\":{{\"active_build_identity\":{{\"display\":{identity}}}}}}}'\n  exit 0\nfi\nexit 1\n"
        ),
    )
    .expect("write fake controller artifact");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("make fake controller artifact executable");
    format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).expect("read fake controller artifact"))
    )
}

#[test]
fn provider_run_result_reads_declared_output_alias() {
    let role_aliases: AgentTaskProviderRoleAliases = serde_json::from_value(json!({
        "outputs": {
            "provider_run_result": ["custom_run_result"]
        }
    }))
    .expect("role aliases");
    let outcome = AgentTaskOutcome {
        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-a".to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: json!({
            "custom_run_result": {
                "run_id": "custom-run-1"
            }
        }),
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };

    assert_eq!(
        provider_run_result(&outcome, &role_aliases)
            .and_then(|result| result.get("run_id"))
            .and_then(Value::as_str),
        Some("custom-run-1")
    );
}

#[test]
fn submit_plan_persists_queued_status() {
    with_isolated_home(|_| {
        let plan = test_plan();

        let record = submit_plan(&plan, Some("run/a")).expect("submitted");
        let loaded = status(&record.run_id).expect("status loaded");

        assert_eq!(record.run_id, "run_a");
        assert_eq!(loaded.state, AgentTaskRunState::Queued);
        assert_eq!(
            loaded.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
                ["requested"],
            crate::build_identity::current().display
        );
        assert!(
            loaded.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
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

#[cfg(unix)]
#[test]
fn artifact_recovery_replaces_only_the_recorded_legacy_pin() {
    with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = crate::build_identity::current().display;
        let artifact = temporary.path().join("exact-homeboy");
        let digest = fake_controller_artifact(&artifact, &identity, "exact artifact");
        let legacy = temporary.path().join("legacy-homeboy");
        std::fs::write(&legacy, b"corrupted legacy bytes").expect("write corrupted legacy pin");
        let record = submit_plan(&test_plan(), Some("recover-exact-artifact")).expect("submit");
        rewrite_record_for_test(&record.run_id, |record| {
            record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                "originating": {
                    "build_identity": identity,
                    "pinned_executable": legacy,
                    "sha256": digest,
                }
            });
        })
        .expect("project legacy pin");

        let recovered = recover_controller_runtime(&record.run_id, Some(&artifact), None)
            .expect("recover exact artifact");
        let pinned = std::path::PathBuf::from(
            recovered["originating"]["pinned_executable"]
                .as_str()
                .expect("recovered pin path"),
        );
        assert_ne!(pinned, legacy);
        assert!(pinned.is_file());
        assert_eq!(
            std::fs::read(&legacy).expect("legacy bytes retained"),
            b"corrupted legacy bytes"
        );
        assert_eq!(
            status(&record.run_id).expect("recovered record").metadata
                [crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY],
            recovered
        );
        validate_controller_runtime(&record.run_id).expect("recovered runtime validates");
    });
}

#[cfg(unix)]
#[test]
fn artifact_recovery_rejects_wrong_hash_and_identity_without_record_mutation() {
    with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = crate::build_identity::current().display;
        let artifact = temporary.path().join("exact-homeboy");
        let digest = fake_controller_artifact(&artifact, &identity, "exact artifact");
        let legacy = temporary.path().join("legacy-homeboy");
        std::fs::write(&legacy, b"corrupted legacy bytes").expect("write corrupted legacy pin");
        let record = submit_plan(&test_plan(), Some("recover-reject-artifact")).expect("submit");

        rewrite_record_for_test(&record.run_id, |record| {
            record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                "originating": {
                    "build_identity": identity,
                    "pinned_executable": legacy,
                    "sha256": "00",
                }
            });
        })
        .expect("project wrong hash pin");
        let before_hash = status(&record.run_id).expect("record before wrong hash");
        let hash_error = recover_controller_runtime(&record.run_id, Some(&artifact), None)
            .expect_err("wrong hash rejected");
        assert!(hash_error.message.contains("hash mismatch"));
        assert_eq!(
            status(&record.run_id).expect("record after wrong hash"),
            before_hash
        );

        rewrite_record_for_test(&record.run_id, |record| {
            record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                "originating": {
                    "build_identity": "homeboy test+wrong-identity",
                    "pinned_executable": legacy,
                    "sha256": digest,
                }
            });
        })
        .expect("project wrong identity pin");
        let before_identity = status(&record.run_id).expect("record before wrong identity");
        let identity_error = recover_controller_runtime(&record.run_id, Some(&artifact), None)
            .expect_err("wrong identity rejected");
        assert!(identity_error.message.contains("build identity mismatch"));
        assert_eq!(
            status(&record.run_id).expect("record after wrong identity"),
            before_identity
        );
    });
}

#[cfg(unix)]
#[test]
fn legacy_v1_pin_migration_failures_leave_durable_record_unchanged() {
    with_isolated_home(|_| {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = crate::build_identity::current().display;
        let record = submit_plan(&test_plan(), Some("migration-failure")).expect("submit");
        let cases = [
            (
                "missing",
                temporary.path().join("missing-homeboy"),
                None,
                "missing",
            ),
            (
                "non-executable",
                temporary.path().join("non-executable-homeboy"),
                Some(identity.clone()),
                "not executable",
            ),
            (
                "identity-mismatch",
                temporary.path().join("wrong-identity-homeboy"),
                Some("homeboy test+wrong".to_string()),
                "build identity mismatch",
            ),
        ];

        for (name, legacy, artifact_identity, expected_error) in cases {
            if let Some(artifact_identity) = artifact_identity {
                fake_controller_artifact(&legacy, &artifact_identity, name);
                if name == "non-executable" {
                    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600))
                        .expect("remove executable permission");
                }
            }
            rewrite_record_for_test(&record.run_id, |record| {
                record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                    "originating": {
                        "build_identity": identity,
                        "pinned_executable": legacy,
                    }
                });
            })
            .expect("project v1 legacy pin");
            let before = status(&record.run_id).expect("record before migration");

            let error = validate_controller_runtime(&record.run_id)
                .expect_err("legacy migration fails closed");

            assert!(
                error.message.contains(expected_error),
                "{name}: {}",
                error.message
            );
            assert_eq!(
                status(&record.run_id).expect("record after migration"),
                before
            );
        }
    });
}

#[cfg(unix)]
#[test]
fn controller_runtime_retention_keeps_mutable_runs_and_reports_terminal_pins_eligible() {
    with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = crate::build_identity::current().display;
        let active_artifact = temporary.path().join("active-homeboy");
        let terminal_artifact = temporary.path().join("terminal-homeboy");
        let active_digest =
            fake_controller_artifact(&active_artifact, &identity, "active artifact");
        let terminal_digest =
            fake_controller_artifact(&terminal_artifact, &identity, "terminal artifact");

        let active = submit_plan(&test_plan(), Some("retention-active")).expect("submit active");
        let terminal =
            submit_plan(&test_plan(), Some("retention-terminal")).expect("submit terminal");
        for (record, artifact, digest) in [
            (&active, &active_artifact, &active_digest),
            (&terminal, &terminal_artifact, &terminal_digest),
        ] {
            let legacy = temporary.path().join(format!("{}-legacy", record.run_id));
            std::fs::write(&legacy, b"corrupted legacy bytes").expect("write legacy pin");
            rewrite_record_for_test(&record.run_id, |record| {
                record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                    "originating": {
                        "build_identity": identity,
                        "pinned_executable": legacy,
                        "sha256": digest,
                    }
                });
            })
            .expect("project legacy pin");
            recover_controller_runtime(&record.run_id, Some(artifact), None).expect("recover pin");
        }
        rewrite_record_for_test(&terminal.run_id, |record| {
            record.state = AgentTaskRunState::Succeeded;
        })
        .expect("make terminal");

        let active_pin = std::path::PathBuf::from(
            status(&active.run_id).expect("active record").metadata
                [crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
                ["pinned_executable"]
                .as_str()
                .expect("active pin"),
        );
        let terminal_pin = std::path::PathBuf::from(
            status(&terminal.run_id).expect("terminal record").metadata
                [crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
                ["pinned_executable"]
                .as_str()
                .expect("terminal pin"),
        );
        let report = crate::controller_runtime::retention_report().expect("retention report");
        assert!(report.retained.contains(&active_pin));
        assert!(report.eligible.contains(&terminal_pin));
        let dry_run = prune_controller_runtime_pins(false).expect("plan pin pruning");
        assert!(dry_run.retained.contains(&active_pin));
        assert!(dry_run.eligible.contains(&terminal_pin));
        assert!(dry_run.removed.is_empty());
        let applied = prune_controller_runtime_pins(true).expect("prune terminal pin");
        assert!(applied.removed.contains(&terminal_pin));
        assert!(active_pin.exists());
        assert!(!terminal_pin.exists());
    });
}

#[test]
fn execution_budget_legacy_plan_migrates_only_for_execution_reads() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("legacy-budget")).expect("submitted");
        let mut raw: Value = serde_json::from_str(
            &std::fs::read_to_string(&record.plan_path).expect("persisted plan"),
        )
        .expect("plan json");
        raw["options"]
            .as_object_mut()
            .expect("schedule options")
            .remove("execution_budget");
        std::fs::write(
            &record.plan_path,
            serde_json::to_vec(&raw).expect("serialize legacy plan"),
        )
        .expect("replace plan");

        let preview = load_plan(&record.run_id).expect("read-only preview");
        assert_eq!(preview.options.execution_budget.version, 0);
        let before = std::fs::read_to_string(&record.plan_path).expect("unmodified preview file");
        assert!(!before.contains("execution_budget"));

        let executed = load_plan_for_execution(&record.run_id).expect("execution migration");
        assert_eq!(executed.options.execution_budget.version, 1);
        let persisted = std::fs::read_to_string(&record.plan_path).expect("migrated plan");
        assert!(persisted.contains("\"version\": 1"));
    });
}

#[test]
fn execution_budget_future_version_fails_closed_without_rewrite() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("future-budget")).expect("submitted");
        let mut raw: Value = serde_json::from_str(
            &std::fs::read_to_string(&record.plan_path).expect("persisted plan"),
        )
        .expect("plan json");
        raw["options"]["execution_budget"]["version"] = json!(99);
        let future = serde_json::to_string_pretty(&raw).expect("serialize future plan");
        std::fs::write(&record.plan_path, &future).expect("replace plan");

        let error = load_plan_for_execution(&record.run_id).expect_err("future version rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(error
            .message
            .contains("unsupported agent-task execution budget version 99"));
        assert_eq!(
            std::fs::read_to_string(&record.plan_path).expect("future plan retained"),
            future
        );
    });
}

#[cfg(unix)]
#[test]
fn submit_plan_persists_owner_only_plan_file_before_observation() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("private-plan")).expect("submitted");

        assert_eq!(
            std::fs::metadata(&record.plan_path)
                .expect("plan metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            status(&record.run_id)
                .expect("observation record")
                .plan_path,
            record.plan_path
        );
    });
}

#[test]
fn active_pinned_run_does_not_block_controller_promotion() {
    with_isolated_home(|_| {
        submit_plan(&test_plan(), Some("active-pinned-runtime")).expect("submitted");

        // Promotion no longer drains durable work. The record owns its pinned
        // runtime and remains available while later admissions switch.
        crate::controller_runtime::activate_current_generation()
            .expect("active durable run must not block promotion");
        let after = submit_plan(&test_plan(), Some("post-promotion-runtime"))
            .expect("post-switch submission");
        assert_eq!(after.state, AgentTaskRunState::Queued);
    });
}

#[test]
fn pinned_runtime_recovery_retains_the_existing_lab_proxy_identity() {
    with_isolated_home(|_| {
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "runtime-a-lab-proxy",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace",
            remote_command: &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run".to_string(),
            ],
            durable_plan: None,
        })
        .expect("runtime A created proxy");
        rewrite_record_for_test("runtime-a-lab-proxy", |record| {
            record.metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
                ["originating"]["build_identity"] = json!("homeboy runtime-a");
        })
        .expect("record runtime A provenance");

        let pinned = pinned_runtime_for_mutation("runtime-a-lab-proxy")
            .expect("runtime B resolves the verified runtime A pin")
            .expect("runtime B delegates to runtime A");
        let record = status("runtime-a-lab-proxy").expect("same durable proxy remains");

        assert!(pinned.is_file());
        assert_eq!(record.run_id, "runtime-a-lab-proxy");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(list_records().expect("one durable record").len(), 1);
        assert!(record.provider_handles.is_empty());
    });
}

#[test]
fn detached_lab_handoff_persists_inspectable_running_record() {
    with_isolated_home(|_| {
        for (run_id, handoff) in [
            ("agent-task-detached-cook", "cook"),
            ("agent-task-detached-batch", "cook-batch"),
            ("agent-task-detached-retry", "run-plan"),
        ] {
            let command = vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                handoff.to_string(),
            ];
            let record = record_detached_lab_run(DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "job-123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            })
            .expect("detached handoff recorded");

            let loaded = status(run_id).expect("status resolves");
            let log = logs(run_id).expect("logs resolve");
            let artifacts = artifacts(run_id).expect("artifacts resolve");

            assert_eq!(record.run_id, run_id);
            assert_eq!(loaded.state, AgentTaskRunState::Running);
            assert_eq!(loaded.tasks[0].state, AgentTaskState::Running);
            assert_eq!(loaded.metadata["runner_id"], "homeboy-lab");
            assert_eq!(loaded.metadata["runner_job_id"], "job-123");
            assert!(loaded.metadata.get("stale_running").is_none());
            assert!(loaded.lifecycle.heartbeat.is_some());
            assert_eq!(
                loaded
                    .lifecycle
                    .heartbeat
                    .as_ref()
                    .map(|heartbeat| heartbeat.last_seen_at.as_str()),
                loaded.updated_at.as_deref()
            );
            assert_eq!(log.events.len(), 1);
            assert!(artifacts.evidence_refs.is_empty());
        }
    });
}

#[test]
fn detached_lab_run_plan_uses_one_identity_for_status_logs_artifacts_and_cancellation() {
    with_isolated_home(|_| {
        let run_id = "agent-task-detached-run-plan";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--record-run-id".to_string(),
            run_id.to_string(),
        ];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8341",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("detached run-plan is bound to the controller run");

        let status = status(run_id).expect("controller identity resolves status");
        let logs = logs(run_id).expect("controller identity resolves logs");
        let artifacts = artifacts(run_id).expect("controller identity resolves artifacts");
        assert_eq!(status.run_id, run_id);
        assert_eq!(status.metadata["runner_job_id"], "job-8341");
        assert!(!logs.events.is_empty());
        assert!(artifacts.artifacts.is_empty());

        let _cancel =
            super::cancellation::test_cancel_hook::install(Box::new(|runner_id, job_id| {
                assert_eq!(runner_id, "homeboy-lab");
                assert_eq!(job_id, "job-8341");
                Ok((
                    crate::api_jobs::Job {
                        id: uuid::Uuid::new_v4(),
                        operation: "runner.exec".to_string(),
                        status: crate::api_jobs::JobStatus::Cancelled,
                        created_at_ms: 1,
                        updated_at_ms: 2,
                        started_at_ms: Some(1),
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
            }));
        let cancelled = cancel_run(run_id, Some("operator requested cancellation"))
            .expect("canonical cancellation reaches the runner job");
        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            cancelled.metadata["live_cancellation"]["cancellation"],
            "runner_job_cancel"
        );
    });
}

#[test]
fn controller_proxy_is_queued_before_handoff_then_binds_runner_child() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let planned = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "agent-task-controller-proxy",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("controller proxy recorded before handoff");

        assert_eq!(planned.state, AgentTaskRunState::Queued);
        assert!(planned.metadata.get("runner_job_id").is_none());
        assert_eq!(planned.metadata["lifecycle_store_owner"], "controller");
        assert_eq!(planned.metadata["handoff_acceptance"]["state"], "pending");
        assert!(load_plan("agent-task-controller-proxy")
            .expect("proxy plan")
            .tasks[0]
            .inputs
            .get("runner_job_id")
            .is_none());
        assert_eq!(
            planned.metadata["runner_execution_record"]["status"],
            "planned"
        );
        assert_eq!(
            logs("agent-task-controller-proxy")
                .expect("logs resolve")
                .events
                .len(),
            1
        );

        let running = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-controller-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("accepted child binds proxy");
        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.metadata["runner_job_id"], "job-123");
        assert_eq!(running.metadata["lifecycle_store_owner"], "controller");
        assert_eq!(running.metadata["handoff_acceptance"]["state"], "accepted");
        assert_eq!(
            running.metadata["runner_execution_record"]["status"],
            "running"
        );
    });
}

#[test]
fn status_expires_an_unaccepted_handoff_but_late_runner_acceptance_wins() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("controller proxy recorded before handoff");
        rewrite_record_for_test("expired-handoff-late-acceptance", |record| {
            record.metadata["handoff_acceptance"]["deadline_at"] =
                json!("2000-01-01T00:00:00+00:00");
        })
        .expect("expire acceptance deadline");

        let expired = status("expired-handoff-late-acceptance")
            .expect("status reconciles the expired controller proxy");
        assert_eq!(expired.state, AgentTaskRunState::Cancelled);
        assert_eq!(expired.metadata["handoff_acceptance"]["state"], "expired");
        assert_eq!(expired.metadata["retryable"], true);

        let accepted = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            runner_job_id: "job-accepted-after-deadline",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("late acceptance supersedes only the synthetic expiry cancellation");
        assert_eq!(accepted.state, AgentTaskRunState::Running);
        assert_eq!(accepted.metadata["handoff_acceptance"]["state"], "accepted");
        assert_eq!(
            accepted.metadata["runner_job_id"],
            "job-accepted-after-deadline"
        );
        assert_eq!(
            accepted.metadata["runner_execution_record"]["status"],
            "running"
        );
    });
}

#[test]
fn detached_cook_attempt_proxy_advances_after_daemon_acceptance() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let attempt_run_id = "cook-7970-attempt-1-controller";
        let queued = record_lab_offload_phase(
            attempt_run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&test_plan()),
        )
        .expect("pre-acceptance attempt record");

        assert_eq!(queued.state, AgentTaskRunState::Queued);
        assert_eq!(queued.metadata["phase"], "materializing");
        assert!(queued.metadata.get("runner_job_id").is_none());

        let accepted = record_detached_lab_run(DetachedLabRunRecord {
            run_id: attempt_run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-7970",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
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
    });
}

#[test]
fn detached_cook_preacceptance_failure_terminalizes_attempt_proxy() {
    with_isolated_home(|_| {
        let attempt_run_id = "cook-7970-attempt-1-staging-failure";
        let plan = test_plan();
        record_lab_offload_phase(
            attempt_run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("pre-acceptance attempt record");

        record_pre_execution_failure(
            attempt_run_id,
            &plan,
            "lab_workspace_stage",
            &Error::internal_unexpected("workspace materialization failed").with_hint(format!(
                "Retry: homeboy agent-task retry {attempt_run_id} --run"
            )),
        )
        .expect("terminal staging failure");

        let record = status(attempt_run_id).expect("terminal attempt record");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "lab_workspace_stage"
        );
        assert!(record.metadata["pre_execution_failure"]["hints"]
            .as_array()
            .expect("failure hints")
            .iter()
            .any(|hint| hint
                == "Retry: homeboy agent-task retry cook-7970-attempt-1-staging-failure --run"));
        assert!(record.metadata.get("runner_job_id").is_none());
    });
}

#[test]
fn missing_lab_attempt_plan_is_recovered_before_handoff_or_terminalized() {
    with_isolated_home(|_| {
        let run_id = "cook-8096-attempt-1";
        let plan = test_plan();
        let record = record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("controller attempt persisted");
        std::fs::remove_file(&record.plan_path).expect("remove interrupted plan");

        let recovered = record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "dispatching",
            Some("/runner/workspace/homeboy"),
            None,
            None,
            Some(&plan),
        )
        .expect("controller plan recovery");
        assert_eq!(load_plan(run_id).expect("recovered plan"), plan);

        std::fs::remove_file(&recovered.plan_path).expect("remove unrecoverable plan");
        let error = record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8096",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &[],
        })
        .expect_err("handoff without plan must not become running");
        assert_eq!(error.code, ErrorCode::InternalIoError);

        let terminal = status(run_id).expect("terminal recovery record");
        assert_eq!(terminal.state, AgentTaskRunState::Failed);
        assert_eq!(
            terminal.metadata["pre_execution_failure"]["phase"],
            "lab_attempt_plan_recovery"
        );
        assert!(terminal.metadata.get("runner_job_id").is_none());
    });
}

#[test]
fn failed_lab_handoff_retry_recovers_the_materialized_user_plan() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.plan_id = "materialized-cook-plan".to_string();
        plan.tasks[0].instructions = "implement the original user task".to_string();
        plan.tasks[0].workspace.root = Some("/materialized/worktree".to_string());
        plan.rebuild_homeboy_plan();
        let record = record_lab_offload_phase(
            "failed-lab-cook",
            "homeboy-lab",
            "materializing",
            Some("pending"),
            None,
            None,
            Some(&plan),
        )
        .expect("controller records user plan before pending handoff");
        let persisted = load_plan(&record.run_id).expect("persisted user plan");
        record_pre_execution_failure(
            &record.run_id,
            &persisted,
            "lab_handoff",
            &Error::internal_unexpected("runner daemon restarted"),
        )
        .expect("terminal handoff failure");

        let retry = retry(&record.run_id, Some("failed-lab-cook-retry")).expect("retry record");
        let recovered = load_plan(&retry.run_id).expect("recovered plan");

        assert_eq!(recovered, plan);
        assert_eq!(
            recovered.tasks[0].workspace.root.as_deref(),
            Some("/materialized/worktree")
        );
        assert!(!serde_json::to_string(&recovered)
            .expect("plan json")
            .contains("pending"));
        assert_eq!(retry.metadata["retry_origin"]["runner_id"], "homeboy-lab");
        assert_eq!(
            retry.metadata["retry_origin"]["remote_workspace"],
            "pending"
        );
        assert_eq!(
            retry.metadata["retry_origin"]["pre_execution_failure"]["phase"],
            "lab_handoff"
        );
    });
}

#[test]
fn retry_uses_controller_plan_when_runner_projection_replaces_plan_path() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let record =
            submit_plan(&plan, Some("runner-projected-retry")).expect("controller plan submitted");
        let mut projected = store::read_record(&record.run_id).expect("source record");
        projected.plan_path =
            "/home/chubes/.local/share/homeboy/agent-task-runs/runner-projected-retry/plan.json"
                .to_string();
        store::write_record(&projected).expect("runner projection mirrored");

        let retry_record = retry(&record.run_id, Some("runner-projected-retry-local"))
            .expect("local retry uses controller plan");
        assert_eq!(load_plan(&retry_record.run_id).expect("retry plan"), plan);

        std::fs::remove_file(record.plan_path).expect("remove authoritative controller plan");
        let error = retry(&record.run_id, Some("missing-controller-plan"))
            .expect_err("missing controller plan fails closed");
        assert_eq!(error.code, ErrorCode::InternalIoError);
    });
}

#[test]
fn status_repairs_runner_plan_projection_and_missing_controller_plan_fails_closed() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let record =
            submit_plan(&plan, Some("runner-projected-status")).expect("controller plan submitted");
        rewrite_record_for_test(&record.run_id, |projected| {
            projected.plan_path =
                "/runner/agent-task-runs/runner-projected-status/plan.json".to_string();
        })
        .expect("runner transport path projected");

        assert_eq!(
            status(&record.run_id).expect("controller status").plan_path,
            record.plan_path
        );
        assert_eq!(
            store::read_record(&record.run_id)
                .expect("repaired record")
                .plan_path,
            record.plan_path
        );

        std::fs::remove_file(&record.plan_path).expect("remove controller plan");
        let error = status(&record.run_id).expect_err("missing controller plan fails closed");
        assert_eq!(error.code, ErrorCode::InternalIoError);
        let diagnostic = error.details["error"]
            .as_str()
            .expect("structured ownership diagnostic");
        assert!(diagnostic.contains("authoritative controller-owned plan"));
        assert!(diagnostic.contains("runner execution transport"));
    });
}

#[test]
fn cook_lab_handoff_controller_reads_ignore_runner_plan_projection() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
        ];
        let record = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "cook-lab-attempt",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace",
            remote_command: &command,
            durable_plan: Some(&plan),
        })
        .expect("cook handoff persists its controller plan");
        let aggregate = succeeded_aggregate(&plan);
        record_run_aggregate(&record.run_id, &plan, &aggregate)
            .expect("runner result mirrored to the controller");
        rewrite_record_for_test(&record.run_id, |record| {
            record.plan_path =
                "/home/chubes/.local/share/homeboy/agent-task-runs/cook-lab-attempt/plan.json"
                    .to_string();
            record.state = AgentTaskRunState::Running;
        })
        .expect("runner transport projection replaces display path");

        assert_eq!(
            status(&record.run_id).expect("controller status").plan_id,
            plan.plan_id
        );
        assert_eq!(
            logs(&record.run_id).expect("controller logs").run_id,
            record.run_id
        );
        assert_eq!(
            artifacts(&record.run_id)
                .expect("controller artifacts")
                .run_id,
            record.run_id
        );
        let retry = retry(&record.run_id, Some("cook-lab-retry"))
            .expect("controller retry uses its durable plan");
        assert_eq!(
            load_controller_plan(&retry.run_id).expect("retry plan"),
            plan
        );

        std::fs::remove_file(record.plan_path)
            .expect("remove authoritative controller plan despite projected display path");
        rewrite_record_for_test(&record.run_id, |record| {
            record.state = AgentTaskRunState::Running;
        })
        .expect("restore active handoff projection");
        let error = status(&record.run_id).expect_err("missing controller plan fails closed");
        assert_eq!(error.code, ErrorCode::InternalIoError);
    });
}

#[test]
fn controller_proxy_records_pre_execution_phase_progress() {
    with_isolated_home(|_| {
        let source = json!({
            "branch": "main",
            "head": "abc123",
        });
        let materializing = record_lab_offload_phase(
            "agent-task-pre-execution",
            "homeboy-lab",
            "materializing",
            None,
            Some(&source),
            Some(&json!({"entries": [{"model": "openai/gpt-5.6-terra"}]})),
            None,
        )
        .expect("materialization phase persisted");

        assert_eq!(materializing.state, AgentTaskRunState::Queued);
        assert_eq!(materializing.metadata["phase"], "materializing");
        assert_eq!(materializing.metadata["provider_state"], "pending");
        assert_eq!(materializing.metadata["source_checkout"]["head"], "abc123");
        assert_eq!(
            materializing.metadata["provider_rotation"]["entries"][0]["model"],
            "openai/gpt-5.6-terra"
        );
        assert!(materializing.metadata.get("runner_job_id").is_none());

        let hydrating = record_lab_offload_phase(
            "agent-task-pre-execution",
            "homeboy-lab",
            "hydrating",
            Some("/runner/workspace/repo"),
            Some(&source),
            None,
            None,
        )
        .expect("hydration phase persisted");
        assert_eq!(hydrating.metadata["phase"], "hydrating");
        assert_eq!(
            hydrating.metadata["remote_workspace"],
            "/runner/workspace/repo"
        );

        let loaded = status("agent-task-pre-execution").expect("status resolves during setup");
        assert_eq!(loaded.metadata["phase"], "hydrating");
        assert_eq!(loaded.metadata["provider_state"], "pending");
        let phases = loaded.metadata["phase_history"]
            .as_array()
            .expect("phase history");
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0]["phase"], "materializing");
        assert!(phases[0].get("started_at").is_some());
        assert!(phases[0].get("ended_at").is_some());
        assert_eq!(phases[1]["phase"], "hydrating");
        assert!(phases[1].get("started_at").is_some());
    });
}

#[test]
fn logs_expose_mirrored_live_runner_events_before_terminal_aggregate() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "live-runner-events",
            runner_id: "homeboy-lab",
            runner_job_id: "job-live",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("running proxy");
        record.metadata["runner_job_events"] = json!([JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::new_v4(),
            kind: JobEventKind::Progress,
            timestamp_ms: 42,
            message: Some("provider started".to_string()),
            data: Some(json!({"provider": "openai/gpt-5.6-terra"})),
        }]);
        store::write_record(&record).expect("persist mirrored event");

        let log = logs("live-runner-events").expect("live logs resolve");

        assert_eq!(log.events.len(), 1);
        assert!(log.events[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider started")));
    });
}

#[test]
fn slow_materialization_remains_discoverable_with_source_identity_and_is_idempotent() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let mut durable_plan = test_plan();
        durable_plan.tasks[0].task_id = "https://github.com/example/project/issues/42".to_string();
        durable_plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: "https://github.com/example/project/issues/42".to_string(),
            revision: None,
        }];
        let started = std::time::Instant::now();
        let first = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        })
        .expect("proxy persisted before staging");

        // Deliberately exceed a caller's short wait budget after the durable
        // write, as a workspace/dependency materializer can in production.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(started.elapsed() > std::time::Duration::from_millis(1));

        let visible = status("slow-materialization").expect("immediately discoverable");
        assert_eq!(visible.run_id, first.run_id);
        assert_eq!(
            visible.tasks[0].task_id,
            "https://github.com/example/project/issues/42"
        );

        let resumed = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        })
        .expect("resume does not duplicate staging record");
        assert_eq!(resumed.run_id, first.run_id);
        let persisted = load_plan("slow-materialization").expect("one persisted plan");
        assert_eq!(persisted.tasks.len(), 1);
        assert_eq!(
            persisted.tasks[0].source_refs[0].uri,
            "https://github.com/example/project/issues/42"
        );

        let with_child = record_lab_offload_phase_executions(
            "slow-materialization",
            "hydrating",
            ["runner-job-42".to_string()],
        )
        .expect("child staging job recorded");
        assert_eq!(
            with_child.metadata["materialization_execution_ids"],
            json!(["runner-job-42"])
        );
    });
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
        let mut record = status("agent-task-terminal-proxy").expect("status");
        apply_runner_job_terminal_state(&mut record, crate::api_jobs::JobStatus::Succeeded, &[]);
        store::write_record(&record).expect("terminal record");

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

#[test]
fn terminal_lab_artifact_attachment_skips_missing_controller_plan_and_preserves_runner_identity() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-late-artifact",
            runner_id: "homeboy-lab",
            runner_job_id: "job-original",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut record = status("agent-task-late-artifact").expect("status");
        apply_runner_job_terminal_state(&mut record, crate::api_jobs::JobStatus::Succeeded, &[]);
        store::write_record(&record).expect("terminal record");
        std::fs::remove_file(&record.plan_path).expect("remove controller plan");

        let attached = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-late-artifact",
            runner_id: "homeboy-lab",
            runner_job_id: "job-artifact-attach",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact",
            remote_command: &command,
        })
        .expect("late attachment leaves terminal proxy intact");

        assert_eq!(attached.state, AgentTaskRunState::Succeeded);
        assert_eq!(attached.runner_id(), Some("homeboy-lab"));
        assert_eq!(attached.runner_job_id(), Some("job-original"));
    });
}

#[test]
fn terminal_lab_artifact_attachment_refuses_runner_provenance_mismatch() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-late-artifact-mismatch",
            runner_id: "homeboy-lab",
            runner_job_id: "job-original",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact-mismatch",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut record = status("agent-task-late-artifact-mismatch").expect("status");
        apply_runner_job_terminal_state(&mut record, crate::api_jobs::JobStatus::Succeeded, &[]);
        store::write_record(&record).expect("terminal record");

        let error = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-late-artifact-mismatch",
            runner_id: "different-lab",
            runner_job_id: "job-artifact-attach",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact-mismatch",
            remote_command: &command,
        })
        .expect_err("artifact provenance must retain its original runner");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "run_id");
    });
}

#[test]
fn disconnected_proxy_projects_terminal_child_aggregate_once_reachable() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut record = status("agent-task-disconnected-child").expect("status");
        let heartbeat = record
            .lifecycle
            .heartbeat
            .clone()
            .expect("confirmed heartbeat");

        record.annotate_runner_disconnected();
        assert_eq!(record.metadata["runner_liveness"], "disconnected");
        assert_eq!(
            record.metadata["stale_running_reason"],
            "runner_disconnected"
        );
        assert_eq!(record.lifecycle.heartbeat, Some(heartbeat));

        let child_plan = test_plan();
        let mut child_aggregate = succeeded_aggregate(&child_plan);
        child_aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
            "patch",
            "patch",
            None,
            Some("/runner/artifacts/patch.diff"),
        )];
        child_aggregate.outcomes[0].diagnostics = vec![AgentTaskDiagnostic {
            class: "provider.attempt".to_string(),
            message: "attempt 1 succeeded".to_string(),
            data: json!({ "attempt": 1 }),
        }];
        let snapshot = terminal_child_snapshot(&child_aggregate);

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("terminal reconciliation");
        let once = record.clone();
        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("repeated reconciliation");

        assert_eq!(
            record, once,
            "repeated terminal reconciliation is idempotent"
        );
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(record.artifact_refs[0].uri, "/runner/artifacts/patch.diff");
        assert_eq!(record.metadata["runner_job_status"], "succeeded");
        assert_eq!(record.metadata["runner_liveness"], "reachable");
        let aggregate = store::read_aggregate("agent-task-disconnected-child")
            .expect("projected child aggregate");
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "provider.attempt"
        );
    });
}

#[test]
fn reachable_running_child_clears_disconnected_liveness_and_refreshes_heartbeat() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-reconnected-running",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        record.annotate_runner_disconnected();
        let disconnected_heartbeat = record.lifecycle.heartbeat.clone();

        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = crate::api_jobs::JobStatus::Running;
        snapshot.events.clear();
        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("reachable reconciliation");

        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(record.metadata["runner_liveness"], "reachable");
        assert!(record.metadata.get("stale_running").is_none());
        assert!(record.metadata.get("stale_running_reason").is_none());
        assert!(record.metadata.get("retryable").is_none());
        assert_ne!(record.lifecycle.heartbeat, disconnected_heartbeat);
    });
}

#[test]
fn accepted_handoff_projects_remote_failure_and_cancellation_without_a_lifecycle_event() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, job_status, expected_state) in [
            (
                "agent-task-remote-failure",
                crate::api_jobs::JobStatus::Failed,
                AgentTaskRunState::Failed,
            ),
            (
                "agent-task-remote-cancellation",
                crate::api_jobs::JobStatus::Cancelled,
                AgentTaskRunState::Cancelled,
            ),
        ] {
            let mut record = record_detached_lab_run(DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            })
            .expect("accepted handoff");
            let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
            snapshot.job.status = job_status;
            snapshot.events.clear();

            reconcile_runner_job_snapshot(&mut record, &snapshot)
                .expect("terminal daemon result reconciles");

            assert_eq!(record.state, expected_state);
            assert_eq!(record.metadata["runner_job_status"], json!(job_status));
        }
    });
}

#[test]
fn accepted_handoff_projects_a_remote_timeout_aggregate_even_when_daemon_transport_succeeds() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-remote-timeout",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("accepted handoff");
        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.status = AgentTaskAggregateStatus::Failed;
        aggregate.totals = AgentTaskAggregateTotals {
            timed_out: 1,
            ..Default::default()
        };
        aggregate.outcomes[0].status = AgentTaskOutcomeStatus::Timeout;
        aggregate.events[0].state = AgentTaskState::Failed;

        let mut snapshot = terminal_child_snapshot(&aggregate);
        let identity = snapshot.events[0]
            .data
            .as_mut()
            .and_then(|event| event.get_mut("identity"))
            .and_then(Value::as_object_mut)
            .expect("terminal lifecycle identity");
        identity.insert("persisted_run_id".to_string(), json!(record.run_id));
        identity.insert("run_id".to_string(), json!(record.run_id));

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect("remote timeout aggregate reconciles");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(
            record.totals.as_ref().map(|totals| totals.timed_out),
            Some(1)
        );
        assert_eq!(record.metadata["runner_job_status"], "succeeded");
    });
}

#[test]
fn running_child_snapshot_persists_provider_handle_and_live_log_progress() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-live-provider",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = crate::api_jobs::JobStatus::Running;
        snapshot.events = vec![crate::api_jobs::JobEvent {
            sequence: 1,
            job_id: snapshot.job.id,
            kind: crate::api_jobs::JobEventKind::Progress,
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

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("live reconciliation");

        assert_eq!(record.metadata["phase"], "executing");
        assert_eq!(record.metadata["provider_state"], "active");
        assert_eq!(record.provider_handles.len(), 1);
        assert_eq!(
            record.provider_handles[0].provider_run_id,
            "provider-run-live"
        );
        let log = logs(&record.run_id).expect("live logs");
        assert_eq!(log.events.len(), 1);
        assert!(log.events[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider dispatch accepted")));
    });
}

#[test]
fn terminal_child_projection_rejects_mismatched_persisted_run_identity() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let before = record.clone();
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["persisted_run_id"] =
            json!("another-controller-run");

        let error = reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect_err("mismatched child must be rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(record, before);
        assert!(store::read_aggregate(&record.run_id).is_err());
    });
}

#[test]
fn terminal_projection_keeps_prior_commit_when_interrupted_before_commit() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let before = record.clone();
        let snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        store::fail_next_record_write_for_test();

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect_err("controller persistence failure is surfaced");

        assert_eq!(record, before);
        let persisted = status(&record.run_id).expect("persisted controller record");
        assert_eq!(persisted.state, AgentTaskRunState::Running);
        assert!(persisted.artifact_refs.is_empty());
        assert!(store::read_aggregate(&record.run_id).is_err());
    });
}

#[test]
fn terminal_projection_is_reader_complete_when_interrupted_after_commit_and_retry_is_idempotent() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        store::interrupt_after_terminal_commit_for_test();

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect_err("interruption after the committed envelope is surfaced");

        assert_eq!(
            store::read_record("agent-task-disconnected-child")
                .expect("committed controller projection")
                .state,
            AgentTaskRunState::Succeeded
        );
        let (status_record, log, artifacts) = std::thread::scope(|scope| {
            let status_reader = scope.spawn(|| status("agent-task-disconnected-child"));
            let log_reader = scope.spawn(|| logs("agent-task-disconnected-child"));
            let artifact_reader = scope.spawn(|| artifacts("agent-task-disconnected-child"));
            (
                status_reader
                    .join()
                    .expect("status reader")
                    .expect("committed status"),
                log_reader
                    .join()
                    .expect("log reader")
                    .expect("committed log"),
                artifact_reader
                    .join()
                    .expect("artifact reader")
                    .expect("committed artifacts"),
            )
        });
        assert_eq!(status_record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert!(artifacts.artifacts.is_empty());

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("idempotent retry");
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert!(store::aggregate_path(&record.run_id)
            .expect("aggregate path")
            .exists());
    });
}

#[test]
fn terminal_runner_reconciliation_never_resurrects_a_controller_record() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let before = record.clone();
        let terminal = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        reconcile_runner_job_snapshot(&mut record, &terminal).expect("terminal reconciliation");
        let terminal_record = record.clone();

        store::write_record(&before).expect("stale running writer is ignored");
        assert_eq!(
            status(&record.run_id)
                .expect("terminal state remains committed")
                .state,
            AgentTaskRunState::Succeeded
        );

        let mut running = terminal.clone();
        running.job.status = crate::api_jobs::JobStatus::Running;
        running.events.clear();
        reconcile_runner_job_snapshot(&mut record, &running)
            .expect("terminal records stay immutable");

        assert_eq!(record, terminal_record);
    });
}

#[test]
fn late_inner_aggregate_recovers_patch_after_transport_only_terminal_result() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");

        // The outer daemon envelope has no captured patch, so its terminal
        // transport result alone cannot represent the inner agent-task output.
        let mut transport_only = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        transport_only.events.clear();
        reconcile_runner_job_snapshot(&mut record, &transport_only)
            .expect("transport-only terminal result");
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert!(artifacts(&record.run_id)
            .expect("transport-only artifacts")
            .artifacts
            .is_empty());

        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "inner-patch".to_string(),
            kind: "patch".to_string(),
            name: Some("candidate.patch".to_string()),
            label: None,
            role: Some("patch".to_string()),
            semantic_key: None,
            path: Some("artifacts/candidate.patch".to_string()),
            url: Some("homeboy://agent-task/run/agent-task-disconnected-child/artifacts#task=task-a&artifact=inner-patch".to_string()),
            mime: Some("text/x-diff".to_string()),
            size_bytes: Some(18_928),
            sha256: Some("062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071".to_string()),
            metadata: Value::Null,
        });
        let lifecycle_snapshot = terminal_child_snapshot(&aggregate);
        reconcile_runner_job_snapshot(&mut record, &lifecycle_snapshot)
            .expect("late inner lifecycle evidence is adopted");

        let artifacts = artifacts(&record.run_id).expect("controller-visible artifacts");
        assert_eq!(artifacts.artifacts.len(), 1);
        assert_eq!(artifacts.artifacts[0].id, "inner-patch");
        assert_eq!(artifacts.artifacts[0].size_bytes, Some(18_928));
        assert_eq!(
            artifacts.artifacts[0].sha256.as_deref(),
            Some("062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071")
        );
        assert_eq!(
            store::read_aggregate(&record.run_id)
                .expect("persisted inner aggregate")
                .outcomes[0]
                .artifacts[0]
                .sha256
                .as_deref(),
            Some("062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071")
        );
    });
}

#[test]
fn terminal_proxy_reconciliation_hydrates_persisted_nested_result_idempotently() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-persisted-result",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        record.plan_path =
            "/home/lab/.local/share/homeboy/agent-task-runs/agent-task-persisted-result/plan.json"
                .to_string();
        store::write_record(&record).expect("runner-local plan projection");
        apply_runner_job_terminal_state(&mut record, crate::api_jobs::JobStatus::Succeeded, &[]);
        store::write_record(&record).expect("legacy terminal projection without aggregate");

        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.outcomes[0].artifacts = vec![AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "final-patch".to_string(),
            kind: "patch".to_string(),
            name: Some("final.patch".to_string()),
            label: None,
            role: Some("patch".to_string()),
            semantic_key: None,
            path: Some("artifacts/final.patch".to_string()),
            url: None,
            mime: Some("text/x-diff".to_string()),
            size_bytes: Some(18_928),
            sha256: Some(
                "062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071".to_string(),
            ),
            metadata: json!({ "source_snapshot": "snapshot-1" }),
        }];
        aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "homeboy://lab/transcript".to_string(),
            label: Some("Provider transcript".to_string()),
        }];
        aggregate.outcomes[0].metadata = json!({
            "provider": "opencode",
            "provider_run_id": "provider-run-1",
        });
        let snapshot = persisted_terminal_result_snapshot(&aggregate);

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("hydrate persisted result");
        let status = status(&record.run_id).expect("hydrated status without runner plan access");
        let artifact_report = artifacts(&record.run_id).expect("hydrated artifacts");
        assert_eq!(status.state, AgentTaskRunState::Succeeded);
        assert_eq!(artifact_report.artifacts.len(), 1);
        assert_eq!(artifact_report.artifacts[0].id, "final-patch");
        assert_eq!(artifact_report.artifacts[0].size_bytes, Some(18_928));
        assert_eq!(
            artifact_report.artifacts[0].sha256.as_deref(),
            Some("062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071")
        );
        assert!(artifact_report
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "transcript"));

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("idempotent replay");
        assert_eq!(
            artifacts(&record.run_id)
                .expect("replayed artifacts")
                .artifacts
                .len(),
            1
        );
    });
}

#[test]
fn terminal_proxy_reconciliation_hydrates_persisted_dispatch_terminal_states() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, aggregate_status, outcome_status, expected_state) in [
            (
                "agent-task-persisted-dispatch-failure",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskRunState::Failed,
            ),
            (
                "agent-task-persisted-dispatch-timeout",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Timeout,
                AgentTaskRunState::Failed,
            ),
        ] {
            let mut record = record_detached_lab_run(DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            })
            .expect("running proxy");
            let mut aggregate = succeeded_aggregate(&test_plan());
            aggregate.status = aggregate_status;
            aggregate.outcomes[0].status = outcome_status;
            aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: format!("homeboy://lab/{run_id}/transcript"),
                label: Some("Provider transcript".to_string()),
            }];

            reconcile_runner_job_snapshot(
                &mut record,
                &persisted_terminal_result_snapshot(&aggregate),
            )
            .expect("hydrate persisted dispatch result");

            let artifact_report = artifacts(run_id).expect("controller artifacts");
            assert_eq!(record.state, expected_state);
            assert!(artifact_report
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == "transcript"));
        }
    });
}

#[test]
fn disconnected_runner_marks_nonterminal_proxy_stale_without_advancing_heartbeat() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-running",
            runner_id: "homeboy-lab",
            runner_job_id: "job-789",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
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
    });
}

#[test]
fn controller_proxy_becomes_terminal_when_handoff_fails_before_child_creation() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "agent-task-handoff-failure",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned proxy");
        let plan = load_plan("agent-task-handoff-failure").expect("proxy plan");
        record_pre_execution_failure(
            "agent-task-handoff-failure",
            &plan,
            "lab_handoff",
            &Error::internal_unexpected("runner rejected handoff"),
        )
        .expect("handoff failure recorded");

        let record = status("agent-task-handoff-failure").expect("terminal status");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.metadata["runner_id"], "homeboy-lab");
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "lab_handoff"
        );
    });
}

#[test]
fn transport_proxy_snapshot_reconciliation_advances_queued_lifecycle() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned proxy");
        let job_id = "00000000-0000-0000-0000-000000000123";
        let metadata = record.ensure_metadata_object();
        metadata.insert("runner_job_id".to_string(), json!(job_id));
        metadata.insert(
            "runner_execution_record".to_string(),
            serde_json::to_value(
                crate::runner_execution_envelope::RunnerExecutionRecord::in_flight(
                    job_id,
                    "homeboy-lab",
                    "daemon",
                )
                .with_job_id(job_id),
            )
            .expect("execution record"),
        );

        let aggregate = succeeded_aggregate(&test_plan());
        reconcile_transport_proxy_snapshot(&mut record, &terminal_child_snapshot(&aggregate))
            .expect("transport proxy reconciliation");

        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(record.metadata["runner_job_status"], "succeeded");
        assert_eq!(
            record.metadata["runner_execution_record"]["status"],
            "succeeded"
        );
    });
}

#[test]
fn recovery_preserves_terminal_runner_identity_before_projecting_runner_artifacts() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let run_id = "agent-task-recovered-runner-artifact";
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "runner/a:lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff");
        record.ensure_metadata_object().remove("runner_id");
        record.ensure_metadata_object().remove("runner_job_id");

        let patch = "runner patch";
        let finalized = crate::paths::artifact_root()
            .expect("artifact root")
            .join("executor-finalized")
            .join(run_id)
            .join("patch.diff");
        std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
            .expect("create finalized parent");
        std::fs::write(&finalized, patch).expect("write finalized patch");
        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/home/runner/.homeboy/executor-finalized/patch.diff".to_string()),
            url: Some(
                "homeboy://agent-task/run/detached-run/artifacts#task=task-a&artifact=patch"
                    .to_string(),
            ),
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(format!("{:x}", sha2::Sha256::digest(patch.as_bytes()))),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
        let mut snapshot = terminal_child_snapshot(&aggregate);
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["runner_id"] =
            json!("runner/a:lab");
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["run_id"] = json!(run_id);
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["persisted_run_id"] =
            json!(run_id);
        let event = crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(
            Some(&snapshot.events),
        )
        .expect("terminal lifecycle event");
        assert_eq!(event.identity.runner_id, "runner/a:lab");
        assert_eq!(
            event.identity.runner_job_id,
            "00000000-0000-0000-0000-000000000123"
        );
        assert_eq!(event.identity.run_id.as_deref(), Some(run_id));
        assert_eq!(event.identity.persisted_run_id.as_deref(), Some(run_id));

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("terminal recovery");

        assert_eq!(record.metadata["runner_id"], "runner/a:lab");
        assert_eq!(
            record.metadata["runner_job_id"],
            "00000000-0000-0000-0000-000000000123"
        );
        assert_eq!(
            record.metadata["artifact_projection"]["status"], "complete",
            "{:#}",
            record.metadata
        );
        let artifacts = crate::observation::ObservationStore::open_initialized()
            .expect("store")
            .list_artifacts(run_id)
            .expect("artifact projections");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "file");
        let projected = verified_controller_artifact_projection_path(
            run_id,
            &aggregate.outcomes[0].task_id,
            &aggregate.outcomes[0].artifacts[0],
        );
        assert_eq!(
            projected.expect("verify projection"),
            Some(std::path::PathBuf::from(&artifacts[0].path))
        );
        assert_ne!(
            artifacts[0].path,
            "/home/runner/.homeboy/executor-finalized/patch.diff"
        );
    });
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
        metadata: json!({ "runner_id": "homeboy-lab", "runner_job_id": "job-123" }),
    };
    record.tasks[0].state = AgentTaskState::Running;

    apply_runner_job_terminal_state(&mut record, crate::api_jobs::JobStatus::Failed, &[]);

    assert_eq!(record.state, AgentTaskRunState::Failed);
    assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
    assert_eq!(record.lifecycle.execution.state, RunExecutionState::Failed);
    assert_eq!(record.metadata["runner_job_status"], "failed");
    assert_eq!(record.metadata["retryable"], true);
}

#[test]
fn detached_lab_handoff_upgrades_existing_observation_record() {
    with_isolated_home(|_| {
        let store =
            crate::observation::ObservationStore::open_initialized().expect("observation store");
        store
            .upsert_imported_run(&crate::observation::RunRecord {
                id: "agent-task-detached".to_string(),
                kind: "agent-task".to_string(),
                component_id: Some("homeboy".to_string()),
                started_at: "2026-07-12T00:00:00Z".to_string(),
                finished_at: None,
                status: "running".to_string(),
                command: Some("homeboy agent-task cook".to_string()),
                cwd: None,
                homeboy_version: Some("test".to_string()),
                git_sha: None,
                rig_id: None,
                metadata_json: json!({
                    "lab": {
                        "remote_job_id": "job-123",
                        "remote_job_status": "running"
                    }
                }),
            })
            .expect("pre-existing observation");

        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-detached",
            runner_id: "homeboy-lab",
            runner_job_id: "job-123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("detached handoff recorded");

        let loaded = status("agent-task-detached").expect("typed status resolves");
        let observation = store
            .get_run("agent-task-detached")
            .expect("read observation")
            .expect("observation exists");
        assert_eq!(loaded.state, AgentTaskRunState::Running);
        assert_eq!(observation.metadata_json["lab"]["remote_job_id"], "job-123");
        assert!(observation.metadata_json.get("agent_task_run").is_some());
    });
}

#[test]
fn terminal_executor_artifacts_are_projected_under_logical_ids() {
    with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("executor artifact root");
        let patch = root.path().join("patch.diff");
        std::fs::write(&patch, "patch bytes").expect("write patch");
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some(patch.display().to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(11),
            sha256: Some(crate::artifact_metadata::sha256_file(&patch).expect("sha")),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
        submit_plan(&plan, Some("projection-parity")).expect("submit");
        record_run_aggregate("projection-parity", &plan, &aggregate).expect("record aggregate");

        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        let artifact = crate::observation::runs_service::resolve_artifact_for_run(
            &store,
            "projection-parity",
            "patch",
        )
        .expect("resolve logical patch id");
        assert_eq!(artifact.run_id, "projection-parity");
        assert_eq!(artifact.kind, "patch");
        assert_eq!(
            std::fs::read(&artifact.path).expect("projected bytes"),
            b"patch bytes"
        );
        let fetched = crate::observation::runs_service::copy_local_file_artifact(
            crate::observation::runs_service::resolve_artifact_for_run(
                &store,
                "projection-parity",
                "patch",
            )
            .expect("resolve runs artifact token"),
            Some(root.path().join("retrieved.patch")),
        )
        .expect("retrieve projected artifact");
        assert_eq!(
            std::fs::read(fetched.output_path).expect("retrieved bytes"),
            b"patch bytes"
        );
    });
}

#[test]
fn terminal_reconciliation_reuses_verified_directly_imported_artifact() {
    with_isolated_home(|home| {
        let patch = b"patch bytes";
        let source = home.path().join("imported.patch");
        std::fs::write(&source, patch).expect("write imported patch");
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
        let submitted = submit_plan(&plan, Some("direct-import-reconciliation")).expect("submit");
        record_runner_job_identity(&submitted.run_id, "homeboy-lab", "job-1")
            .expect("runner identity");

        let mut hash = sha2::Sha256::new();
        sha2::Digest::update(&mut hash, submitted.run_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, aggregate.outcomes[0].task_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, b"patch");
        let artifact_id = format!("agent-task-{:x}", hash.finalize());
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        store
            .import_artifact(&crate::observation::ArtifactRecord {
                id: artifact_id,
                run_id: submitted.run_id.clone(),
                kind: "patch".to_string(),
                artifact_type: "file".to_string(),
                path: source.display().to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
                size_bytes: Some(patch.len() as i64),
                mime: Some("text/x-patch".to_string()),
                metadata_json: json!({ "name": "patch" }),
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .expect("direct artifact import");

        record_run_aggregate(&submitted.run_id, &plan, &aggregate)
            .expect("terminal reconciliation");
        reconcile_terminal_artifact_projection(&submitted.run_id).expect("repeated reconciliation");

        let record = store::read_record(&submitted.run_id).expect("terminal record");
        assert_eq!(record.metadata["artifact_projection"]["status"], "complete");
        let artifact = crate::observation::runs_service::resolve_artifact_for_run(
            &store,
            &submitted.run_id,
            "patch",
        )
        .expect("actionable imported patch");
        let output = home.path().join("recovered.patch");
        crate::observation::runs_service::copy_local_file_artifact(artifact, Some(output.clone()))
            .expect("recover patch without runner");
        assert_eq!(std::fs::read(output).expect("recovered patch bytes"), patch);
    });
}

#[test]
fn terminal_reconciliation_rejects_conflicting_directly_imported_artifact() {
    with_isolated_home(|home| {
        let patch = b"patch bytes";
        let conflicting = b"other bytes";
        let source = home.path().join("conflicting.patch");
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
        let submitted = submit_plan(&plan, Some("direct-import-conflict")).expect("submit");
        record_runner_job_identity(&submitted.run_id, "homeboy-lab", "job-1")
            .expect("runner identity");

        let mut hash = sha2::Sha256::new();
        sha2::Digest::update(&mut hash, submitted.run_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, aggregate.outcomes[0].task_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, b"patch");
        let artifact_id = format!("agent-task-{:x}", hash.finalize());
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        store
            .import_artifact(&crate::observation::ArtifactRecord {
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

        record_run_aggregate(&submitted.run_id, &plan, &aggregate)
            .expect("terminal state is persisted");
        let record = store::read_record(&submitted.run_id).expect("terminal record");
        assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
        assert!(record.metadata["artifact_projection"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("conflicts with terminal artifact projection")));
    });
}

#[test]
fn status_backfills_legacy_runner_provenance_and_mirrors_a_verified_projection_idempotently() {
    with_isolated_home(|_| {
        use sha2::Digest;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let patch = b"patch bytes";
        let sha256 = format!("{:x}", sha2::Sha256::digest(patch));
        let response_sha256 = sha256.clone();
        let listener = TcpListener::bind("127.0.0.1:0").expect("runner daemon listener");
        let address = listener.local_addr().expect("runner daemon address");
        std::thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().expect("runner daemon request");
                let mut request = [0; 1024];
                let read = stream.read(&mut request).expect("read runner request");
                if read == 0 {
                    continue;
                }
                let request = String::from_utf8_lossy(&request[..read]);
                if request.starts_with("GET /runs/detached-run/artifacts/patch/content ") {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Homeboy-Artifact-Sha256: {response_sha256}\r\nConnection: close\r\n\r\n{}",
                        patch.len(),
                        String::from_utf8_lossy(patch),
                    )
                    .expect("write artifact response");
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .expect("write jobs response");
                }
            }
        });
        let session = homeboy_lab_runner_contract::RunnerSession {
            runner_id: "local".to_string(),
            mode: homeboy_lab_runner_contract::RunnerTunnelMode::DirectSsh,
            role: homeboy_lab_runner_contract::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: Some(address.port()),
            local_url: Some(format!("http://{address}")),
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: now_timestamp(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        };
        let session_path = crate::paths::runner_session_file("local").expect("session path");
        std::fs::create_dir_all(session_path.parent().expect("session parent"))
            .expect("create session parent");
        std::fs::write(
            session_path,
            serde_json::to_string(&session).expect("session JSON"),
        )
        .expect("write session");
        struct FakeRunnerEvidence;
        impl crate::observation::runs_service::RunnerEvidenceProvider for FakeRunnerEvidence {
            fn mirror_connected_runner_run(
                &self,
                _: &str,
            ) -> Result<Option<crate::observation::RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<crate::observation::runs_service::RunnerConnectionInfo> {
                Vec::new()
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_job_cancel(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
                unreachable!()
            }
            fn refresh_mirrored_daemon_evidence(
                &self,
                _: &str,
            ) -> Result<Option<Vec<crate::observation::RunRecord>>> {
                Ok(None)
            }
            fn mirrored_runner_job_identity(
                &self,
                _: &crate::observation::RunRecord,
            ) -> Option<(String, String)> {
                None
            }
            fn download_remote_artifact(
                &self,
                path: &str,
                output: Option<std::path::PathBuf>,
            ) -> Result<crate::observation::runs_service::RemoteArtifactDownloadInfo> {
                assert_eq!(path, "runner-artifact://local/detached-run/patch");
                let output_path = output.unwrap_or_else(|| {
                    crate::paths::artifact_root()
                        .expect("artifact root")
                        .join("fake-runner-patch")
                });
                std::fs::write(&output_path, b"patch bytes").expect("write fake runner bytes");
                Ok(
                    crate::observation::runs_service::RemoteArtifactDownloadInfo {
                        output_path,
                        content_type: Some("text/x-patch".to_string()),
                        size_bytes: Some(11),
                        sha256: Some(format!("{:x}", sha2::Sha256::digest(b"patch bytes"))),
                        artifact_ref: homeboy_lab_runner_contract::RunnerArtifactRef {
                            artifact_id: "patch".to_string(),
                            name: None,
                            path: Some(path.to_string()),
                            url: None,
                            mime: Some("text/x-patch".to_string()),
                            size_bytes: Some(11),
                            sha256: Some(format!("{:x}", sha2::Sha256::digest(b"patch bytes"))),
                            transport: Some("test".to_string()),
                        },
                    },
                )
            }
        }
        crate::observation::runs_service::register_runner_evidence_provider(Box::new(
            FakeRunnerEvidence,
        ));

        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/home/runner/.homeboy/executor-finalized/patch.diff".to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(sha256),
            metadata: json!({
                "executor_artifact_finalized": true,
                "source_provenance": { "runner_id": "local" }
            }),
        });
        submit_plan(&plan, Some("detached-run")).expect("submit");
        record_runner_job_identity("detached-run", "local", "job-1").expect("runner setup");
        record_run_aggregate("detached-run", &plan, &aggregate).expect("reconcile aggregate");

        // Match the pre-#8562 persisted shape: terminal and claimed complete,
        // but without a controller projection or record-level runner identity.
        let mut legacy = store::read_record("detached-run").expect("legacy record");
        legacy.ensure_metadata_object().remove("runner_id");
        legacy.ensure_metadata_object().remove("runner_job_id");
        legacy.ensure_metadata_object().insert(
            "artifact_projection".to_string(),
            json!({ "status": "complete" }),
        );
        store::write_record(&legacy).expect("persist legacy record");
        let observation =
            crate::observation::ObservationStore::open_initialized().expect("observation store");
        for artifact in observation
            .list_artifacts("detached-run")
            .expect("existing projections")
        {
            observation
                .delete_artifact_record(&artifact.id)
                .expect("remove unverified projection");
        }

        let record = status("detached-run").expect("status");
        assert_eq!(record.metadata["runner_id"], "local");
        assert_eq!(record.metadata["artifact_projection"]["status"], "complete");
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        let artifact = crate::observation::runs_service::resolve_artifact_for_run(
            &store,
            "detached-run",
            "patch",
        )
        .expect("controller projection");
        assert_eq!(artifact.artifact_type, "file");
        assert_eq!(
            std::fs::read(artifact.path).expect("projection bytes"),
            patch
        );
        let projection_count = store
            .list_artifacts("detached-run")
            .expect("initial projections")
            .len();
        let replay = status("detached-run").expect("idempotent status");
        assert_eq!(replay.metadata, record.metadata);
        assert_eq!(
            store
                .list_artifacts("detached-run")
                .expect("idempotent projections")
                .len(),
            projection_count
        );
    });
}

#[test]
fn controller_leaves_runner_artifact_projection_pending_when_it_cannot_mirror_bytes() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts = vec![
            AgentTaskArtifact {
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some("/runner/private/one.patch".to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: Some(3),
                sha256: Some("one".to_string()),
                metadata: json!({ "executor_artifact_finalized": true }),
            },
            AgentTaskArtifact {
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "report".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some("/runner/private/two.json".to_string()),
                url: None,
                mime: Some("application/json".to_string()),
                size_bytes: Some(3),
                sha256: Some("two".to_string()),
                metadata: json!({ "executor_artifact_finalized": true }),
            },
        ];
        let submitted = submit_plan(&plan, Some("projection/run with space")).expect("submit");
        record_runner_job_identity(&submitted.run_id, "runner/a:lab", "job-1")
            .expect("runner identity");
        record_run_aggregate(&submitted.run_id, &plan, &aggregate).expect("controller projection");

        let record = status(&submitted.run_id).expect("status");
        assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
        assert!(record.metadata["artifact_projection"]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        let remote_alias = crate::observation::runs_service::resolve_artifact_for_run(
            &store,
            &submitted.run_id,
            "patch",
        )
        .expect("runner artifact alias remains available");
        assert_eq!(remote_alias.artifact_type, "remote_file");
        assert!(crate::execution_contract::is_remote_runner_artifact_path(
            &remote_alias.path
        ));
        assert_eq!(
            verified_controller_artifact_projection_path(
                &submitted.run_id,
                &aggregate.outcomes[0].task_id,
                &aggregate.outcomes[0].artifacts[0],
            )
            .expect("verify controller projection"),
            None,
        );
    });
}

#[test]
fn running_observation_projects_each_terminal_aggregate_state() {
    with_isolated_home(|_| {
        let cases = [
            (
                "terminal-success",
                AgentTaskAggregateStatus::Succeeded,
                AgentTaskOutcomeStatus::Succeeded,
                "succeeded",
            ),
            (
                "terminal-failure",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Failed,
                "failed",
            ),
            (
                "terminal-partial",
                AgentTaskAggregateStatus::PartialFailure,
                AgentTaskOutcomeStatus::Failed,
                "partial_failure",
            ),
            (
                "terminal-cancelled",
                AgentTaskAggregateStatus::Cancelled,
                AgentTaskOutcomeStatus::Cancelled,
                "cancelled",
            ),
        ];
        for (run_id, aggregate_status, outcome_status, terminal_state) in cases {
            let plan = test_plan();
            let mut aggregate = succeeded_aggregate(&plan);
            aggregate.status = aggregate_status;
            aggregate.outcomes[0].status = outcome_status;
            submit_plan(&plan, Some(run_id)).expect("submit");
            mark_running(run_id).expect("running");
            record_run_aggregate(run_id, &plan, &aggregate).expect("terminal aggregate");

            let observation = crate::observation::ObservationStore::open_initialized()
                .expect("store")
                .get_run(run_id)
                .expect("observation")
                .expect("existing running observation transitioned");
            assert_ne!(observation.status, "running");
            assert_eq!(
                observation.metadata_json["agent_task_terminal_state"],
                terminal_state
            );
        }
    });
}

#[test]
fn cook_index_keeps_repeated_attempts_unique_with_stable_latest_alias() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = succeeded_aggregate(&plan);
        let first_run_id = cook_attempt_run_id("cook-issue-6978", 1);
        let second_run_id = cook_attempt_run_id("cook-issue-6978", 1);

        assert_ne!(first_run_id, second_run_id);

        record_completed_run(&plan, &aggregate, Some(&first_run_id)).expect("first run recorded");
        record_cook_attempt("cook-issue-6978", 1, &first_run_id).expect("first cook indexed");
        record_completed_run(&plan, &aggregate, Some(&second_run_id)).expect("second run recorded");
        record_cook_attempt("cook-issue-6978", 1, &second_run_id).expect("second cook indexed");

        let index = cook_index("cook-issue-6978").expect("cook index loaded");
        assert_eq!(index.latest_run_id, second_run_id);
        assert_eq!(index.attempts.len(), 2);
        assert_eq!(index.attempts[0].run_id, first_run_id);
        assert_eq!(index.attempts[1].run_id, second_run_id);

        let latest = status("cook-issue-6978").expect("stable cook id resolves");
        assert_eq!(latest.run_id, second_run_id);
        assert_eq!(latest.metadata["cook_alias"], "cook-issue-6978");
        assert_eq!(
            latest.metadata["cook_index"]["latest_run_id"],
            second_run_id
        );

        let (_raw, path) = aggregate_source("cook-issue-6978").expect("latest aggregate resolves");
        assert!(path.display().to_string().contains(&second_run_id));
    });
}

#[test]
fn record_promotion_persists_latest_event_on_run_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-promotion-status")).expect("submitted");

        let promotion = json!({
            "schema": "homeboy/agent-task-promotion-status/v1",
            "status": "applied",
            "source_run_id": "run-promotion-status",
            "patch_artifact_id": "patch.diff",
            "to_worktree": "homeboy@fix-5055",
            "target": {
                "worktree": "homeboy@fix-5055",
                "branch": "fix/5055",
                "head": "abc123"
            },
            "operator_notification": {
                "status": "completed",
                "message": "patch promoted into homeboy@fix-5055"
            }
        });

        let updated = record_promotion("run-promotion-status", promotion.clone())
            .expect("promotion recorded");
        let loaded = status("run-promotion-status").expect("status loaded");

        assert_eq!(updated.metadata["latest_promotion"], promotion);
        assert_eq!(
            loaded.metadata["latest_promotion"]["patch_artifact_id"],
            "patch.diff"
        );
        assert_eq!(
            loaded.metadata["promotions"]
                .as_array()
                .expect("events")
                .len(),
            1
        );
    });
}

#[test]
fn corrected_promotion_replaces_gate_failed_latest_proof() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let run_id = "run-corrected-promotion";
        submit_plan(&plan, Some(run_id)).expect("submitted");

        let gate_failed = json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "gate_failed",
            "source": { "kind": "aggregate", "task_id": "task-a", "run_id": run_id },
            "to_worktree": "homeboy@fix-8307",
            "target": { "worktree": "homeboy@fix-8307", "path": "/repo" },
            "patch_artifact": { "id": "first.patch", "kind": "patch", "path": "first.patch" },
            "changed_files": ["src/lib.rs"],
            "gate_results": [{ "id": "test", "name": "cargo test", "kind": "command", "status": "failed" }],
            "provenance": { "candidate": { "kind": "git", "fingerprint": { "schema": "homeboy/agent-task-candidate-fingerprint/v1", "target_path": "/repo", "head": "base", "base": "base", "changed_files": ["src/lib.rs"], "sha256": "first" } } },
            "operator_notification": { "status": "blocked", "message": "gates failed" }
        });
        let corrected = json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": { "kind": "aggregate", "task_id": "task-a", "run_id": run_id },
            "to_worktree": "homeboy@fix-8307",
            "target": { "worktree": "homeboy@fix-8307", "path": "/repo" },
            "patch_artifact": { "id": "corrected.patch", "kind": "patch", "path": "corrected.patch" },
            "changed_files": ["src/lib.rs"],
            "gate_results": [{ "id": "test", "name": "cargo test", "kind": "command", "status": "passed" }],
            "provenance": { "candidate": { "kind": "git", "fingerprint": { "schema": "homeboy/agent-task-candidate-fingerprint/v1", "target_path": "/repo", "head": "base", "base": "base", "changed_files": ["src/lib.rs"], "sha256": "corrected" } } },
            "operator_notification": { "status": "completed", "message": "gates passed" }
        });

        record_promotion(run_id, gate_failed).expect("gate failure recorded");
        let updated = record_promotion(run_id, corrected.clone()).expect("correction recorded");

        let latest: crate::agent_task_promotion::AgentTaskPromotionReport =
            serde_json::from_value(updated.metadata["latest_promotion"].clone())
                .expect("latest promotion is finalization proof");
        assert_eq!(
            latest.status,
            crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
        );
        assert_eq!(latest.patch_artifact.id, "corrected.patch");
        assert_eq!(
            updated.metadata["promotions"]
                .as_array()
                .expect("history")
                .len(),
            2
        );
    });
}

#[test]
fn pre_dispatch_failure_persists_failed_run_without_provider_handle() {
    with_isolated_home(|_| {
        let record = record_pre_dispatch_failure(AgentTaskPreDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "cook-lab-predispatch",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-lab-predispatch".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--cwd".to_string(),
                    "/runner/workspace/repo".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                failure_message: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout",
                stdout: "",
                stderr: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout\n",
                exit_code: 1,
            })
            .expect("pre-dispatch failure recorded");

        let loaded = status("cook-lab-predispatch").expect("status loaded");
        let log = logs("cook-lab-predispatch").expect("logs loaded");
        let artifact_report = artifacts("cook-lab-predispatch").expect("artifacts loaded");
        let legacy_status_path = crate::paths::homeboy_data()
            .expect("homeboy data")
            .join("agent-task-runs")
            .join("cook-lab-predispatch")
            .join("status.json");
        std::fs::remove_file(
            crate::paths::homeboy_data()
                .expect("homeboy data")
                .join("agent-task-runs")
                .join("cook-lab-predispatch")
                .join("aggregate.json"),
        )
        .expect("aggregate file removed");
        let mirrored_log = logs("cook-lab-predispatch").expect("mirrored logs loaded");
        let mirrored_artifacts =
            artifacts("cook-lab-predispatch").expect("mirrored artifacts loaded");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert!(loaded.provider_handles.is_empty());
        assert_eq!(log.events[1].state, AgentTaskState::Failed);
        assert_eq!(mirrored_log.events[1].state, AgentTaskState::Failed);
        assert_eq!(loaded.metadata["provider_run_ids"], serde_json::json!([]));
        assert_eq!(
            loaded.artifact_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            artifact_report.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            mirrored_artifacts.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert!(
            !legacy_status_path.exists(),
            "agent-task status.json is no longer the primary durable run record"
        );
    });
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
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "logs".to_string(),
                    uri: "homeboy://agent-task/run/remote-run/logs".to_string(),
                    label: Some("remote provider logs".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: serde_json::json!({
                    "provider_run_result": {
                        "status": "failed",
                        "failure_classification": "runtime",
                        "artifacts": [],
                        "refs": { "logs": [], "transcripts": [], "runtimes": [] }
                    }
                }),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "remote_run_id": "provider-run-1",
                    "remote_workspace": "/runner/workspace/repo"
                }),
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

        let loaded = status("local-run").expect("status loaded");
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
            log.events[0].message.as_deref(),
            Some("Remote provider agent task failed.")
        );
        assert_eq!(artifacts.evidence_refs[0].kind, "logs");
        assert!(raw_aggregate.contains("fixture.agent-task-executor"));
        assert!(raw_aggregate.contains("failure_classification"));
    });
}

#[test]
fn aggregate_only_remote_dispatch_failure_preserves_lab_outcome_details() {
    with_isolated_home(|_| {
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "remote-plan".to_string(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-conductor".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "provider-run".to_string(),
                    uri: "homeboy://provider/runs/provider-run-1".to_string(),
                    label: Some("Provider run".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: serde_json::json!({
                    "provider_run_result": {
                        "schema": "custom-provider/agent-task-run-result/v1",
                        "run_id": "provider-run-1",
                        "status": "failed",
                        "failure_classification": "runtime",
                        "metadata": {
                            "remote_plan_ref": "remote-plan",
                            "remote_run_ref": "remote-run"
                        }
                    }
                }),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "remote_run_id": "provider-run-1",
                }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": "remote-plan",
            "state": "failed",
            "aggregate": aggregate,
        });

        let record = record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "conductor-full-loop-proof-retry2-20260611",
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
                remote_workspace: "/runner/workspace/conductor",
                stdout: &envelope.to_string(),
                stderr: "",
                exit_code: 1,
            },
            &envelope,
        )
        .expect("aggregate-only dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = status("conductor-full-loop-proof-retry2-20260611").expect("status loaded");
        let log = logs("conductor-full-loop-proof-retry2-20260611").expect("logs loaded");
        let artifacts =
            artifacts("conductor-full-loop-proof-retry2-20260611").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("conductor-full-loop-proof-retry2-20260611")
            .expect("aggregate source");

        assert_eq!(record.run_id, "conductor-full-loop-proof-retry2-20260611");
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
        assert_eq!(loaded.provider_handles.len(), 1);
        assert_eq!(loaded.provider_handles[0].provider_run_id, "provider-run-1");
        assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
        assert_eq!(loaded.metadata["remote_plan_path"], "remote-plan");
        assert_eq!(
            log.events[0].message.as_deref(),
            Some("Remote provider agent task failed.")
        );
        assert_eq!(artifacts.evidence_refs[0].kind, "provider-run");
        assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
        assert!(raw_aggregate.contains("failure_classification"));
        assert!(raw_aggregate.contains("remote_plan_ref"));
    });
}

#[test]
fn sparse_aggregate_only_remote_dispatch_failure_adds_remote_evidence_refs() {
    with_isolated_home(|_| {
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "remote-plan".to_string(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-conductor".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: serde_json::json!({}),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "provider_run_result": {
                        "schema": "custom-provider/agent-task-run-result/v1",
                        "status": "failed",
                        "failure_classification": "runtime"
                    }
                }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": "remote-plan",
            "state": "failed",
            "aggregate": aggregate,
        });

        record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "local-sparse-run",
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
                remote_workspace: "/runner/workspace/conductor",
                stdout: "",
                stderr: &envelope.to_string(),
                exit_code: 1,
            },
            &envelope,
        )
        .expect("sparse dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = status("local-sparse-run").expect("status loaded");
        let artifacts = artifacts("local-sparse-run").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("local-sparse-run").expect("aggregate source");

        assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
        assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
        assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "remote-agent-task-logs"));
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "remote-agent-task-review"));
        assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
        assert!(raw_aggregate.contains("failure_classification"));
    });
}

#[test]
fn record_completed_run_exposes_logs_and_artifacts() {
    with_isolated_home(|_| {
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
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("patch.diff".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some("/tmp/patch.diff".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "file:///tmp/transcript.json".to_string(),
                    label: Some("provider transcript".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
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

        let record =
            record_completed_run(&plan, &aggregate, Some("run-complete")).expect("recorded");
        let log = logs(&record.run_id).expect("logs");
        let artifacts = artifacts(&record.run_id).expect("artifacts");

        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert_eq!(artifacts.artifacts[0].id, "patch");
        assert_eq!(artifacts.evidence_refs[0].kind, "transcript");
    });
}

#[test]
fn completed_run_exposes_latest_executor_input_output_and_expectations() {
    with_isolated_home(|_| {
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
        aggregate.outcomes[0].outputs = json!({
            "provider_run_result": {
                "run_id": "provider-run-123",
                "status": "succeeded"
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-evidence")).expect("recorded");
        let evidence = record
            .latest_executor_evidence
            .as_ref()
            .expect("latest executor evidence");
        let artifact_report = artifacts("run-evidence").expect("artifacts loaded");

        assert_eq!(evidence.task_id, "task-a");
        assert_eq!(evidence.backend, "sandbox");
        assert_eq!(evidence.selector.as_deref(), Some("fixture"));
        assert_eq!(evidence.model.as_deref(), Some("gpt-fixture"));
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
    });
}

#[test]
fn completed_generic_executor_outcome_preserves_runtime_evidence_without_provider_run_id() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.tasks[0].executor.model = Some("openai/gpt-5.6-terra".to_string());
        let aggregate = succeeded_aggregate(&plan);

        let record = record_completed_run(&plan, &aggregate, Some("generic-executor-outcome"))
            .expect("recorded");
        let runtime = record
            .lifecycle
            .provider_runtime
            .first()
            .expect("canonical executor runtime evidence");

        assert!(record.provider_handles.is_empty());
        assert_eq!(record.metadata["provider_run_ids"], json!([]));
        assert_eq!(runtime.backend, "opencode");
        assert_eq!(runtime.state, ProviderRuntimeState::Succeeded);
        assert!(runtime.external_runtime_ids.is_empty());
        assert_eq!(
            runtime.metadata["evidence_source"],
            "canonical_executor_outcome"
        );
        assert_eq!(
            runtime.metadata["executor"]["model"],
            "openai/gpt-5.6-terra"
        );
    });
}

#[test]
fn status_preserves_existing_terminal_runtime_evidence() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0].executor.backend = "opencode".to_string();
        let aggregate = succeeded_aggregate(&plan);
        let record = record_completed_run(&plan, &aggregate, Some("existing-runtime"))
            .expect("terminal record");
        rewrite_record_for_test(&record.run_id, |record| {
            record.lifecycle.provider_runtime[0].metadata = json!({
                "evidence_source": "native_provider",
                "manual": true,
            });
        })
        .expect("native evidence persisted");
        let before = store::read_record(&record.run_id).expect("record before status");

        let loaded = status(&record.run_id).expect("status preserves runtime evidence");
        let after = store::read_record(&record.run_id).expect("record after status");

        assert_eq!(before, after);
        assert_eq!(
            loaded.lifecycle.provider_runtime[0].metadata["manual"],
            true
        );
    });
}

#[test]
fn submitted_run_can_be_loaded_marked_running_and_completed() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-execute")).expect("submitted");

        let loaded_plan = load_plan("run-execute").expect("plan loaded");
        let running = mark_running("run-execute").expect("marked running");
        let aggregate = succeeded_aggregate(&loaded_plan);

        let completed =
            record_run_aggregate("run-execute", &loaded_plan, &aggregate).expect("completed");
        let durable_status = status("run-execute").expect("status");

        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.tasks[0].state, AgentTaskState::Running);
        assert_eq!(
            running.lifecycle.execution.state,
            RunExecutionState::Running
        );
        assert!(running.lifecycle.heartbeat.is_some());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(
            completed.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(completed.totals, Some(aggregate.totals.clone()));
        assert_eq!(durable_status.state, AgentTaskRunState::Succeeded);
        assert_eq!(durable_status.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(durable_status.totals, Some(aggregate.totals.clone()));
        assert!(completed.aggregate_path.is_some());
    });
}

#[test]
fn run_state_bridges_one_to_one_onto_execution_state() {
    let cases = [
        (AgentTaskRunState::Queued, RunExecutionState::Queued),
        (AgentTaskRunState::Running, RunExecutionState::Running),
        (AgentTaskRunState::Succeeded, RunExecutionState::Succeeded),
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
fn cancel_keeps_run_state_and_execution_state_paired() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-pairing")).expect("submitted");
        mark_running("run-cancel-pairing").expect("marked running");

        let cancelled = cancel("run-cancel-pairing").expect("cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            cancelled.lifecycle.execution.state,
            RunExecutionState::from(cancelled.state),
        );
        assert_eq!(
            cancelled.lifecycle.execution.state,
            RunExecutionState::Cancelled,
        );
    });
}

#[test]
fn lifecycle_store_round_trips_record_log_artifacts_and_lifecycle_contract() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
            "patch",
            "patch",
            None,
            Some("/tmp/patch.diff"),
        )];
        aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("provider transcript".to_string()),
        }];

        let record = record_completed_run(&plan, &aggregate, Some("run/store-contract"))
            .expect("completed run recorded");
        let loaded = status("run/store-contract").expect("status loaded by unsanitized id");
        let log = logs("run/store-contract").expect("logs loaded by unsanitized id");
        let artifact_report =
            artifacts("run/store-contract").expect("artifacts loaded by unsanitized id");
        let records = list_records().expect("records listed");

        assert_eq!(record.run_id, "run_store-contract");
        assert!(run_record_exists("run/store-contract").expect("record exists"));
        assert_eq!(loaded.state, AgentTaskRunState::Succeeded);
        assert_eq!(loaded.lifecycle.schema, RUN_LIFECYCLE_RECORD_SCHEMA);
        assert_eq!(
            loaded.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(loaded.lifecycle.cleanup.state, CleanupState::Preserved);
        assert_eq!(
            loaded.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::Retained
        );
        assert_eq!(log.schema, schemas::RUN_LOG);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert_eq!(artifact_report.schema, schemas::RUN_ARTIFACTS);
        assert_eq!(artifact_report.artifacts[0].id, "patch");
        assert_eq!(artifact_report.evidence_refs[0].kind, "transcript");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run_store-contract");
    });
}

#[test]
fn completed_run_persists_opaque_provider_handles_from_outcome_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].metadata = json!({
            "provider_handle": AgentTaskExecutionHandle {
                kind: AgentTaskExecutionHandleKind::ProviderRun,
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                run_id: "provider-run-123".to_string(),
                stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                metadata: json!({ "opaque": { "provider_owned": true } }),
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-handle")).expect("recorded");

        assert_eq!(record.provider_handles.len(), 1);
        assert_eq!(record.provider_handles[0].task_id, "task-a");
        assert_eq!(record.provider_handles[0].backend, "sample-runtime");
        assert_eq!(
            record.provider_handles[0].provider_run_id,
            "provider-run-123"
        );
        assert_eq!(
            record.provider_handles[0].stream_uri.as_deref(),
            Some("provider://runs/provider-run-123/events")
        );
        assert_eq!(
            record.provider_handles[0].state,
            Some(AgentTaskState::Succeeded)
        );
        assert_eq!(
            record.provider_handles[0].metadata["opaque"]["provider_owned"],
            json!(true)
        );
        assert_eq!(
            record.metadata["provider_run_ids"],
            json!(["provider-run-123"])
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            ProviderRuntimeState::Succeeded
        );
        assert_eq!(
            record.lifecycle.external_runtime_ids[0].value,
            "provider-run-123"
        );
        assert_eq!(
            record.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::NotApplicable
        );
    });
}

#[test]
fn failed_provider_run_exposes_workflow_evidence_refs() {
    with_isolated_home(|_| {
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
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("provider task failed".to_string()),
                failure_classification: Some(
                    crate::agent_task::AgentTaskFailureClassification::ExecutionFailed,
                ),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
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
                follow_up: None,
                metadata: Value::Null,
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

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-failed")).expect("recorded");
        let durable_status = status(&record.run_id).expect("status");
        let durable_artifacts = artifacts(&record.run_id).expect("artifacts");

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
    });
}

#[test]
fn cancel_marks_queued_run_and_tasks_cancelled() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel")).expect("submitted");

        let record = cancel("run-cancel").expect("cancelled");

        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
        assert!(record.metadata["cancel_requested_at"].is_string());
    });
}

#[test]
fn retry_submits_new_run_from_existing_plan() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-original")).expect("submitted");
        let mut source = store::read_record("run-original").expect("source");
        source.metadata["notification_route"] = json!({
            "transport": "extension",
            "route": "opaque-origin"
        });
        store::write_record(&source).expect("route persisted");

        let record = retry("run-original", Some("run-retry")).expect("retry submitted");
        let loaded_plan = load_plan("run-retry").expect("retry plan loaded");

        assert_eq!(record.run_id, "run-retry");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.metadata["retry_of"], json!("run-original"));
        assert_eq!(
            record.metadata["notification_route"]["route"],
            "opaque-origin"
        );
        assert_eq!(loaded_plan.plan_id, "plan-a");
    });
}

#[test]
fn status_recovers_terminal_state_from_durable_aggregate() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-status")).expect("submitted");
        mark_running("run-stale-status").expect("marked running");
        let aggregate = succeeded_aggregate(&plan);
        store::write_aggregate("run-stale-status", &aggregate).expect("aggregate written");

        let recovered = status("run-stale-status").expect("status recovered");
        let persisted = store::read_record("run-stale-status").expect("record persisted");

        assert_eq!(recovered.state, AgentTaskRunState::Succeeded);
        assert_eq!(recovered.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(recovered.totals, Some(aggregate.totals.clone()));
        assert_eq!(persisted.state, AgentTaskRunState::Succeeded);
        assert_eq!(persisted.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(persisted.totals, Some(aggregate.totals.clone()));
    });
}

#[test]
fn status_marks_running_run_without_owner_as_stale() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-missing-owner")).expect("submitted");
        let mut record = store::read_record("run-stale-missing-owner").expect("record");
        record.state = AgentTaskRunState::Running;
        store::write_record(&record).expect("stored running record");

        let loaded = status("run-stale-missing-owner").expect("status loaded");

        assert_eq!(loaded.state, AgentTaskRunState::Running);
        assert_eq!(loaded.metadata["stale_running"], json!(true));
        assert_eq!(
            loaded.metadata["stale_running_reason"],
            "missing_runner_pid"
        );
        assert_eq!(loaded.metadata["provider_boundary"]["status"], "absent");

        // Read-side reconciliation persists the classification, so repeated
        // status reads converge instead of reviving a ghost run as active.
        let persisted = store::read_record("run-stale-missing-owner").expect("persisted record");
        assert_eq!(persisted.metadata["stale_running"], json!(true));
        let repeated = status("run-stale-missing-owner").expect("repeated status loaded");
        assert_eq!(repeated.metadata["stale_running"], json!(true));
    });
}

#[test]
fn aggregate_source_loads_completed_run_without_path_spelunking() {
    with_isolated_home(|_| {
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
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };
        record_completed_run(&plan, &aggregate, Some("run-source")).expect("recorded");
        let local_path = store::aggregate_path("run-source").expect("local aggregate path");
        let mut record = store::read_record("run-source").expect("record loaded");
        record.aggregate_path = Some("/home/user/remote/aggregate.json".to_string());
        store::write_record(&record).expect("remote aggregate path stored");
        std::fs::remove_file(&local_path).expect("local aggregate removed");

        let (raw, path) = aggregate_source("run-source").expect("aggregate source");

        assert!(path.ends_with("aggregate.json"));
        assert_ne!(path, PathBuf::from("/home/user/remote/aggregate.json"));
        assert!(raw.contains("task-a"));
    });
}

#[test]
fn mark_running_reclaims_stale_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-dead-owner")).expect("submitted");
        let mut record = store::read_record("run-stale-dead-owner").expect("record");
        record.state = AgentTaskRunState::Running;
        record.metadata = json!({ "runner_pid": u32::MAX });
        store::write_record(&record).expect("stored stale record");

        let running = mark_running("run-stale-dead-owner").expect("reclaimed");

        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.metadata["reclaimed_stale_running"], json!(true));
        assert_eq!(running.metadata["runner_pid"], json!(std::process::id()));
    });
}

#[test]
fn mark_running_rejects_live_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-live-owner")).expect("submitted");
        mark_running("run-live-owner").expect("marked running");

        let error = mark_running("run-live-owner").expect_err("live run rejected");

        assert!(error.message.contains("already running"));
    });
}

#[test]
fn cancel_run_marks_queued_record_cancelled() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-queued")).expect("submitted");

        let cancelled =
            cancel_run("run-cancel-queued", Some("loser cell")).expect("queued run cancelled");
        let loaded = status("run-cancel-queued").expect("status loaded");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(cancelled.metadata["cancel_reason"], json!("loser cell"));
        assert_eq!(loaded.state, AgentTaskRunState::Cancelled);
    });
}

#[test]
fn cancel_run_reclaims_stale_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-stale")).expect("submitted");
        let mut record = store::read_record("run-cancel-stale").expect("record");
        record.state = AgentTaskRunState::Running;
        record.tasks[0].state = AgentTaskState::Running;
        record.metadata = json!({ "runner_pid": u32::MAX });
        store::write_record(&record).expect("stored stale record");

        let cancelled = cancel_run("run-cancel-stale", None).expect("stale run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(cancelled.metadata["cancelled_stale_running"], json!(true));
        assert!(cancelled.metadata.get("stale_running").is_none());
    });
}

#[test]
fn cancel_run_signals_live_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-live")).expect("submitted");
        mark_running("run-cancel-live").expect("marked running");

        let cancelled = cancel_run("run-cancel-live", None).expect("live run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(
            cancelled.metadata["live_cancellation"]["owner_pid"],
            json!(std::process::id())
        );
        assert_eq!(
            cancelled.metadata["live_cancellation"]["signal"],
            json!("SIGTERM")
        );
    });
}

#[test]
fn cancel_run_emits_recovery_commands_for_runner_backed_run() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-runner")).expect("submitted");
        let mut record = store::read_record("run-cancel-runner").expect("record");
        record.state = AgentTaskRunState::Running;
        record.tasks[0].state = AgentTaskState::Running;
        // Runner-backed: owner pid lives on the runner host (not running
        // here), so live cancellation must hand back recovery commands.
        record.metadata = json!({
            "runner_pid": u32::MAX,
            "runner_id": "lab-a",
            "runner_job_id": "job-123",
        });
        store::write_record(&record).expect("stored runner record");

        let cancelled = cancel_run("run-cancel-runner", None).expect("runner run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        let unsupported = &cancelled.metadata["live_cancellation_unsupported"];
        assert!(unsupported.is_object());
        assert_eq!(unsupported["runner_id"], json!("lab-a"));
        assert_eq!(unsupported["runner_job_id"], json!("job-123"));
        let commands = unsupported["recovery_commands"]
            .as_array()
            .expect("recovery commands array");
        assert!(!commands.is_empty());
        // The first recovery command should route cancellation to the
        // owning runner so the operator can act deterministically.
        assert!(commands[0]
            .as_str()
            .expect("command string")
            .contains("homeboy runner exec lab-a"));
        // No real local process was signalled.
        assert!(cancelled.metadata.get("live_cancellation").is_none());
    });
}

#[test]
fn list_records_skips_malformed_observation_records() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("good-run")).expect("submitted");
        let store =
            crate::observation::ObservationStore::open_initialized().expect("observation store");
        store
            .upsert_imported_run(&crate::observation::RunRecord {
                id: "bad-run".to_string(),
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
                metadata_json: json!({ "schema": "homeboy/agent-task-observation-record/v1" }),
            })
            .expect("bad record inserted");

        let records = list_records().expect("records listed");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "good-run");
    });
}

#[test]
fn record_health_reconciles_plan_backed_missing_metadata_idempotently() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("repairable-metadata")).expect("submitted");
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        let mut observation = store
            .get_run("repairable-metadata")
            .expect("read")
            .expect("observation");
        observation.metadata_json = json!({ "legacy_source": "fixture" });
        store
            .upsert_imported_run(&observation)
            .expect("malformed fixture stored");

        let before = record_health_summary().expect("health");
        assert_eq!(before.malformed, 1);
        assert_eq!(
            before.samples[0].reason,
            AgentTaskRecordHealthReason::MissingMetadata
        );
        let dry_run = reconcile_record_health(true).expect("dry run");
        assert_eq!(dry_run.records[0].action, "would-migrate");
        assert_eq!(
            record_health_summary()
                .expect("unmodified health")
                .malformed,
            1
        );

        let applied = reconcile_record_health(false).expect("applied");
        assert_eq!(applied.migrated, 1);
        let repaired = status("repairable-metadata").expect("reconstructed record");
        assert_eq!(repaired.state, AgentTaskRunState::Running);
        assert_eq!(
            repaired.metadata["lifecycle_reconstruction"]["original_metadata"]["legacy_source"],
            json!("fixture")
        );
        let healthy = record_health_summary().expect("healthy health");
        assert_eq!(healthy.malformed + healthy.legacy + healthy.conflicting, 0);
        assert_eq!(
            reconcile_record_health(false).expect("repeat").considered,
            0
        );
    });
}

#[test]
fn record_health_migrates_legacy_and_quarantines_conflicting_projections() {
    with_isolated_home(|_| {
        submit_plan(&test_plan(), Some("legacy-record")).expect("submitted");
        rewrite_record_for_test("legacy-record", |record| {
            record.schema = "homeboy/agent-task-run/v0".to_string();
        })
        .expect("legacy stored");
        let legacy = reconcile_record_health(false).expect("legacy migrated");
        assert_eq!(legacy.migrated, 1);
        let legacy_record = status("legacy-record").expect("legacy loaded");
        assert_eq!(legacy_record.schema, schemas::RUN);
        assert!(legacy_record
            .metadata
            .get("lifecycle_reconstruction")
            .is_some());

        submit_plan(&test_plan(), Some("conflicting-record")).expect("submitted");
        rewrite_record_for_test("conflicting-record", |record| {
            record.lifecycle.execution.state = RunExecutionState::Succeeded;
        })
        .expect("conflict stored");
        let dry_run = reconcile_record_health(true).expect("conflict dry run");
        assert_eq!(
            dry_run.records[0].reason,
            AgentTaskRecordHealthReason::ConflictingProjections
        );
        assert_eq!(dry_run.records[0].action, "would-quarantine");
        let applied = reconcile_record_health(false).expect("conflict quarantined");
        assert_eq!(applied.quarantined, 1);
        let health = record_health_summary().expect("quarantine health");
        assert_eq!(health.conflicting, 1);
        assert_eq!(health.quarantined, 1);
        assert_eq!(
            reconcile_record_health(false)
                .expect("repeat no-op")
                .considered,
            0
        );
    });
}

#[test]
fn record_health_recovers_after_interrupted_migration_without_changing_terminal_status() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("interrupted-terminal")).expect("submitted");
        let store = crate::observation::ObservationStore::open_initialized().expect("store");
        let mut observation = store
            .get_run("interrupted-terminal")
            .expect("read")
            .expect("observation");
        observation.status = "pass".to_string();
        observation.finished_at = Some("2026-01-01T00:01:00Z".to_string());
        observation.metadata_json = json!({});
        store
            .upsert_imported_run(&observation)
            .expect("terminal malformed fixture");

        store::fail_next_record_write_for_test();
        assert!(reconcile_record_health(false).is_err());
        assert_eq!(
            record_health_summary().expect("still malformed").malformed,
            1
        );
        let applied = reconcile_record_health(false).expect("retry migration");
        assert_eq!(applied.migrated, 1);
        let repaired = status("interrupted-terminal").expect("repaired");
        assert_eq!(repaired.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            repaired.lifecycle.execution.finished_at.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
    });
}

fn outcome_with_refs(
    task_id: &str,
    artifacts: Vec<AgentTaskArtifact>,
    evidence_refs: Vec<AgentTaskEvidenceRef>,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
        summary: Some("ok".to_string()),
        failure_classification: None,
        artifacts,
        typed_artifacts: Vec::new(),
        evidence_refs,
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

fn artifact_ref_artifact(
    id: &str,
    kind: &str,
    url: Option<&str>,
    path: Option<&str>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: Some(format!("{kind} artifact")),
        label: None,
        role: None,
        semantic_key: None,
        path: path.map(str::to_string),
        url: url.map(str::to_string),
        mime: None,
        size_bytes: None,
        sha256: None,
        metadata: Value::Null,
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
fn artifact_refs_omit_artifacts_with_empty_url_and_path() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![
            artifact_ref_artifact(
                "dir-empty",
                "sample-runtime-artifact-directory",
                Some(""),
                Some(""),
            ),
            artifact_ref_artifact("dir-none", "sample-runtime-agent-task-input", None, None),
            artifact_ref_artifact("patch", "patch", None, Some("/tmp/patch.diff")),
        ],
        Vec::new(),
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1, "artifacts lacking a usable uri are dropped");
    assert_eq!(refs[0].kind, "patch");
    assert_eq!(refs[0].uri, "/tmp/patch.diff");
}

#[test]
fn artifact_refs_treat_empty_url_as_missing_and_fall_back_to_path() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "dir",
            "sample-runtime-artifact-directory",
            Some("   "),
            Some("/tmp/artifacts/dir"),
        )],
        Vec::new(),
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].uri, "/tmp/artifacts/dir");
}

#[test]
fn artifact_refs_dedup_identical_refs_across_artifacts_and_evidence() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "transcript",
            "transcript",
            Some("file:///tmp/transcript.json"),
            None,
        )],
        vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("transcript artifact".to_string()),
        }],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(
        refs.len(),
        1,
        "exact-duplicate refs collapse to a single entry"
    );
}

#[test]
fn status_filters_empty_uri_artifact_refs() {
    with_isolated_home(|_| {
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

        let record =
            record_completed_run(&plan, &aggregate, Some("run-empty-refs")).expect("recorded");
        let durable_status = status(&record.run_id).expect("status");

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
    });
}

#[test]
fn run_status_reports_bridge_envelope_and_cursor_filtered_events() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.events = vec![
            AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Running,
                attempt: 1,
                message: Some("started".to_string()),
            },
            AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            },
        ];
        aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
            "bundle",
            "artifact-bundle",
            Some("file:///tmp/bundle.json"),
            None,
        )];

        record_completed_run(&plan, &aggregate, Some("run-status-bridge")).expect("recorded");

        let status = run_status("run-status-bridge", Some(1)).expect("bridge status");

        assert_eq!(status.schema, schemas::RUN_STATUS);
        assert_eq!(status.state, AgentTaskRunState::Succeeded);
        assert_eq!(status.latest_event_cursor, 2);
        assert_eq!(status.normalized_events.len(), 1);
        assert_eq!(status.normalized_events[0].sequence, 2);
        assert_eq!(status.normalized_events[0].schema, schemas::EVENT);
        assert_eq!(
            status.normalized_events[0].event_type,
            "agent_task.state_changed"
        );
        assert_eq!(status.normalized_events[0].artifact_refs.len(), 1);
        assert_eq!(status.artifact_refs[0].kind, "artifact-bundle");
    });
}

#[test]
fn logs_include_normalized_event_envelopes() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.events = vec![
            AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Running,
                attempt: 1,
                message: Some("started".to_string()),
            },
            AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            },
        ];
        aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("Transcript".to_string()),
        }];

        record_completed_run(&plan, &aggregate, Some("run-event-envelope")).expect("recorded");

        let log = logs("run-event-envelope").expect("logs");

        assert_eq!(log.normalized_events.len(), 2);
        assert_eq!(log.normalized_events[0].schema, schemas::EVENT);
        assert_eq!(log.normalized_events[0].run_id, "run-event-envelope");
        assert_eq!(log.normalized_events[0].task_id, "task-a");
        assert_eq!(log.normalized_events[0].sequence, 1);
        assert_eq!(log.normalized_events[0].status, AgentTaskState::Running);
        assert_eq!(log.normalized_events[1].message.as_deref(), Some("ok"));
        assert_eq!(log.normalized_events[1].artifact_refs.len(), 1);
    });
}

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

fn terminal_child_snapshot(
    aggregate: &AgentTaskAggregate,
) -> crate::api_jobs::RunnerJobLogSnapshot {
    let job_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").expect("job id");
    crate::api_jobs::RunnerJobLogSnapshot {
        job: crate::api_jobs::Job {
            id: job_id,
            operation: "agent-task".to_string(),
            status: crate::api_jobs::JobStatus::Succeeded,
            created_at_ms: 1,
            updated_at_ms: 2,
            started_at_ms: Some(1),
            finished_at_ms: Some(2),
            event_count: 1,
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
        events: vec![crate::api_jobs::JobEvent {
            sequence: 1,
            job_id,
            kind: crate::api_jobs::JobEventKind::Progress,
            timestamp_ms: 2,
            message: Some("agent-task lifecycle event".to_string()),
            data: Some(json!({
                "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                "identity": {
                    "runner_id": "homeboy-lab",
                    "runner_job_id": job_id.to_string(),
                    "persisted_run_id": "agent-task-disconnected-child",
                    "run_id": "agent-task-disconnected-child",
                },
                "aggregate": aggregate,
            })),
        }],
    }
}

fn persisted_terminal_result_snapshot(
    aggregate: &AgentTaskAggregate,
) -> crate::api_jobs::RunnerJobLogSnapshot {
    let mut snapshot = terminal_child_snapshot(aggregate);
    snapshot.events[0].kind = JobEventKind::Result;
    snapshot.events[0].data = Some(json!({
        "exit_code": 0,
        "stdout": format!("HOMEBOY_RUNNER_PROGRESS {{\"phase\":\"finished\"}}\n{}", json!({
            "schema": "homeboy/command-result/v3",
            "command": "agent-task",
            "success": true,
            "exit_code": 0,
            "data": {
                "schema": "homeboy/agent-task-dispatch/v1",
                "aggregate": aggregate,
            },
        }))
    }));
    snapshot
}

fn succeeded_aggregate(plan: &AgentTaskPlan) -> AgentTaskAggregate {
    AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Succeeded,
        totals: AgentTaskAggregateTotals {
            queued: 1,
            succeeded: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }],
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
    }
}
