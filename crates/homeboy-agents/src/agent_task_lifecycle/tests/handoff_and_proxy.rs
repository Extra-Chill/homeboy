//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
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
use homeboy_core::api_jobs::{Job, JobEvent, JobEventKind, JobStore, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[test]
fn submit_plan_persists_queued_status() {
    with_isolated_home(|_| {
        let plan = test_plan();

        let record = submit_plan(&plan, Some("run/a")).expect("submitted");
        let loaded = status(&record.run_id).expect("status loaded");

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

#[cfg(unix)]
#[test]
fn controller_runtime_retention_keeps_mutable_and_retained_terminal_runs() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        // Controller-runtime retention discovers referenced pins through the
        // agent-task pin-reference provider hook; register it so the report can
        // see this test's durable records.
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
        for (record, artifact, digest) in [
            (&active, &active_artifact, &active_digest),
            (&terminal, &terminal_artifact, &terminal_digest),
        ] {
            let legacy = temporary.path().join(format!("{}-legacy", record.run_id));
            std::fs::write(&legacy, b"corrupted legacy bytes").expect("write legacy pin");
            rewrite_record_for_test(&record.run_id, |record| {
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
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
            // Use `set_run_state` so the run state and its lifecycle execution
            // projection stay consistent. A raw `record.state = Succeeded` write
            // left `lifecycle.execution.state` unchanged, so `diagnose_run`
            // flagged the record `ConflictingProjections` and dropped it from
            // `list_records_with_health` — the source the retention report reads —
            // making the terminal pin silently unreferenced (#8964).
            set_run_state(record, AgentTaskRunState::Succeeded);
            record.lifecycle.artifact_retention.status = ArtifactRetentionStatus::Retained;
        })
        .expect("make terminal");

        let active_pin = std::path::PathBuf::from(
            status(&active.run_id).expect("active record").metadata
                [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
                ["pinned_executable"]
                .as_str()
                .expect("active pin"),
        );
        let terminal_pin = std::path::PathBuf::from(
            status(&terminal.run_id).expect("terminal record").metadata
                [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]["originating"]
                ["pinned_executable"]
                .as_str()
                .expect("terminal pin"),
        );
        let report =
            homeboy_core::controller_runtime::retention_report().expect("retention report");
        assert!(report.retained.contains(&active_pin));
        assert!(report.retained.contains(&terminal_pin));
        let dry_run = prune_controller_runtime_pins(false).expect("plan pin pruning");
        assert!(dry_run.retained.contains(&active_pin));
        assert!(dry_run.retained.contains(&terminal_pin));
        assert!(dry_run.removed.is_empty());
        let applied = prune_controller_runtime_pins(true).expect("prune unreferenced pins");
        assert!(!applied.removed.contains(&terminal_pin));
        assert!(active_pin.exists());
        assert!(terminal_pin.exists());
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
    status(run_id).expect("record loaded").metadata
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
        let source_record = status(&source.run_id).expect("source record");
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
fn accepted_handoff_replays_idempotently_and_rejects_a_different_identity() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "immutable-handoff",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned handoff");
        let input = DetachedLabRunRecord {
            run_id: "immutable-handoff",
            runner_id: "homeboy-lab",
            runner_job_id: "job-immutable",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        };
        let accepted = record_detached_lab_run(input.clone()).expect("accepted handoff");
        let replay = record_detached_lab_run(input).expect("idempotent replay");
        assert_eq!(replay, accepted);

        let error = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "immutable-handoff",
            runner_id: "other-runner",
            runner_job_id: "other-job",
            remote_workspace: "/other/workspace",
            remote_command: &command,
        })
        .expect_err("different accepted identity is rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        let stored = status("immutable-handoff").expect("accepted record retained");
        assert_eq!(stored.runner_id(), Some("homeboy-lab"));
        assert_eq!(stored.runner_job_id(), Some("job-immutable"));
    });
}

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
        let stored = status("pending-runner-identity").expect("pending handoff retained");
        assert_eq!(stored, planned);
    });
}

#[test]
fn accepted_proxy_resume_rejects_a_different_runner_without_rewriting_legacy_projection() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "immutable-proxy",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned handoff");
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "immutable-proxy",
            runner_id: "homeboy-lab",
            runner_job_id: "job-proxy",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("accepted handoff");

        let error = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "immutable-proxy",
            runner_id: "other-runner",
            remote_workspace: "/other/workspace",
            remote_command: &command,
            durable_plan: None,
        })
        .expect_err("different runner resume is rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        let stored = status("immutable-proxy").expect("accepted record retained");
        assert_eq!(stored.metadata["runner_id"], "homeboy-lab");
        assert_eq!(stored.metadata["runner_job_id"], "job-proxy");
        assert_eq!(
            stored.metadata["runner_execution_record"]["status"],
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
        apply_runner_job_terminal_state(
            &mut record,
            homeboy_core::api_jobs::JobStatus::Succeeded,
            &[],
        );
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
        snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
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
        running.job.status = homeboy_core::api_jobs::JobStatus::Running;
        running.events.clear();
        reconcile_runner_job_snapshot(&mut record, &running)
            .expect("terminal records stay immutable");

        assert_eq!(record, terminal_record);
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
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
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
fn run_record_exists_resolves_a_cook_id_to_its_latest_run() {
    // #8390: the Lab retry handoff guarded on the exact-match `run_record_exists`,
    // so a resolvable id (e.g. a cook id) reported absent even though `retry`
    // would succeed, and the handoff silently fell through to ship an unrunnable
    // `agent-task retry <id>` to the runner. `run_record_exists_resolved` must
    // report present for a cook id that resolves to a real run.
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = succeeded_aggregate(&plan);
        let run_id = cook_attempt_run_id("cook-issue-8390", 1);
        record_completed_run(&plan, &aggregate, Some(&run_id)).expect("run recorded");
        record_cook_attempt("cook-issue-8390", 1, &run_id).expect("cook indexed");

        // Exact match sees only the concrete run id, not the cook alias.
        assert!(run_record_exists(&run_id).expect("exact run exists"));
        assert!(!run_record_exists("cook-issue-8390").expect("cook id not an exact record"));

        // Resolution-aware existence follows the same path `retry` uses.
        assert!(run_record_exists_resolved(&run_id).expect("resolved run exists"));
        assert!(
            run_record_exists_resolved("cook-issue-8390").expect("cook id resolves"),
            "a cook id must resolve to its latest run for the Lab retry handoff"
        );
        assert!(
            !run_record_exists_resolved("cook-does-not-exist").expect("missing id"),
            "a genuinely missing id must still report absent"
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
fn cancel_run_marks_queued_record_cancelled() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-queued")).expect("submitted");
        let mut record = store::read_record("run-cancel-queued").expect("record");
        record.metadata = json!({
            "runner_id": "homeboy-lab",
            "runner_job_id": "queued-reservation",
        });
        store::write_record(&record).expect("store runner reservation");
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

        let cancelled =
            cancel_run("run-cancel-queued", Some("loser cell")).expect("queued run cancelled");
        let loaded = status("run-cancel-queued").expect("status loaded");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(cancelled.metadata["cancel_reason"], json!("loser cell"));
        assert_eq!(
            cancelled.metadata["live_cancellation"]["cancellation"],
            "runner_job_cancel"
        );
        assert_eq!(loaded.state, AgentTaskRunState::Cancelled);
    });
}

#[test]
fn list_records_skips_malformed_observation_records() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("good-run")).expect("submitted");
        let store = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        store
            .upsert_imported_run(&homeboy_core::observation::RunRecord {
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
fn record_health_summary_stays_bounded_with_many_malformed_records() {
    with_isolated_home(|_| {
        // A state directory full of historical malformed agent-task records must
        // not produce unbounded per-record output. The health summary aggregates
        // every malformed record into a total count while capping the retained
        // samples, so read-only activity/upgrade output stays bounded. (#8397)
        let malformed_count = crate::agent_task_lifecycle::health::HEALTH_SAMPLE_LIMIT * 3;
        let store = homeboy_core::observation::ObservationStore::open_initialized()
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

        let health = record_health_summary().expect("health summary");

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
    });
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
