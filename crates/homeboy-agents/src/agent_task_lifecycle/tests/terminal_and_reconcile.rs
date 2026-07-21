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

#[cfg(unix)]
#[test]
fn artifact_recovery_replaces_only_the_recorded_legacy_pin() {
    with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let artifact = temporary.path().join("exact-homeboy");
        let digest = fake_controller_artifact(&artifact, &identity, "exact artifact");
        let legacy = temporary.path().join("legacy-homeboy");
        std::fs::write(&legacy, b"corrupted legacy bytes").expect("write corrupted legacy pin");
        let record = submit_plan(&test_plan(), Some("recover-exact-artifact")).expect("submit");
        rewrite_record_for_test(&record.run_id, |record| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
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
                [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY],
            recovered
        );
        validate_controller_runtime(&record.run_id).expect("recovered runtime validates");
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
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
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
fn detached_handoff_persists_redacted_submission_intent_before_broker_ack() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "intent-before-post",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("controller proxy");
        record_lab_offload_submission_intent(
            "intent-before-post",
            "homeboy-lab",
            "/runner/workspace/repo",
            &command,
            &["RUNNER_SECRET_TOKEN".to_string()],
        )
        .expect("persist intent");

        let pending = status("intent-before-post").expect("preparing intent status");
        assert_eq!(
            pending.metadata["runner_submission_intent"]["state"],
            "preparing"
        );
        assert_eq!(
            pending.metadata["runner_submission_intent"]["submission_key"],
            "agent-task:v1:homeboy-lab:intent-before-post"
        );
        assert!(pending.metadata["runner_submission_intent"]
            .get("replay_request")
            .is_none());
        assert!(pending
            .lab_handoff
            .as_ref()
            .and_then(|handoff| handoff.payload_fingerprint.as_deref())
            .is_none());
        assert_eq!(pending.metadata["phase"], "waiting_for_runner_capacity");
        assert!(!serde_json::to_string(&pending)
            .expect("serialize record")
            .contains("secret-value"));

        let accepted = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "intent-before-post",
            runner_id: "homeboy-lab",
            runner_job_id: "job-replayed",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("ack binds intent");
        assert_eq!(
            accepted.metadata["runner_submission_intent"]["state"],
            "accepted"
        );
        assert_eq!(
            accepted.metadata["runner_submission_intent"]["runner_job_id"],
            "job-replayed"
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
fn failed_lab_preacceptance_reconstructs_only_authenticated_zero_execution_recovery() {
    with_isolated_home(|_| {
        let run_id = "cook-preacceptance-recovery";
        let plan = test_plan();
        record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("pre-acceptance attempt record");
        record_pre_execution_failure(
            run_id,
            &plan,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected("truncated Lab handoff payload"),
        )
        .expect("terminal preacceptance failure");

        let mut record = status(run_id).expect("failed record");
        record.metadata["phase"] = json!("lab_handoff_preacceptance");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert!(record.aggregate_path.is_some());
        assert!(!record.artifact_refs.is_empty());
        assert_eq!(record.lifecycle.provider_runtime.len(), 1);
        assert_eq!(
            record.lifecycle.provider_runtime[0].metadata["evidence_source"],
            "canonical_executor_outcome"
        );
        assert!(candidate_adoption_recovery_outcome(&record, &plan.tasks[0]).is_some());

        let mut wrong_phase = record.clone();
        wrong_phase.metadata["phase"] = json!("provider_dispatch");
        assert!(candidate_adoption_recovery_outcome(&wrong_phase, &plan.tasks[0]).is_none());

        let mut consumed_execution = record.clone();
        consumed_execution.metadata["provider_executions_consumed"] = json!(1);
        assert!(candidate_adoption_recovery_outcome(&consumed_execution, &plan.tasks[0]).is_none());

        let mut provider_handle = record.clone();
        provider_handle
            .provider_handles
            .push(AgentTaskRunProviderHandle {
                kind: Default::default(),
                task_id: "task-a".to_string(),
                backend: "test".to_string(),
                provider_run_id: "provider-actual-run".to_string(),
                stream_uri: None,
                state: Some(AgentTaskState::Failed),
                metadata: Value::Null,
            });
        assert!(candidate_adoption_recovery_outcome(&provider_handle, &plan.tasks[0]).is_none());

        let mut runner_job = record.clone();
        runner_job.metadata["runner_job_id"] = json!("job-actual-provider");
        assert!(candidate_adoption_recovery_outcome(&runner_job, &plan.tasks[0]).is_none());

        let mut provider_runtime = record.clone();
        provider_runtime.lifecycle.provider_runtime[0]
            .external_runtime_ids
            .push(homeboy_core::run_lifecycle_record::ExternalRuntimeId {
                kind: "provider_run_id".to_string(),
                value: "provider-actual-run".to_string(),
                provider: Some("test".to_string()),
                url: None,
            });
        assert!(candidate_adoption_recovery_outcome(&provider_runtime, &plan.tasks[0]).is_none());

        let mut changed_recovery = record;
        changed_recovery.metadata["pre_execution_failure"]["candidate_adoption_recovery"]
            ["reason"] = json!("provider_failure");
        assert!(candidate_adoption_recovery_outcome(&changed_recovery, &plan.tasks[0]).is_none());
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
fn long_pre_submission_setup_survives_reconciliation_and_terminal_phase_writes_are_noops() {
    with_isolated_home(|_| {
        let run_id = "long-pre-submission-materialization";
        let plan = test_plan();
        let materializing = record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "materializing",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("persist pre-submission materialization");

        // A setup phase may run longer than the handoff lease because no complete
        // request has crossed the durable submission boundary yet.
        assert!(materializing.lab_handoff.is_none());
        assert!(materializing.metadata.get("handoff_acceptance").is_none());
        assert_eq!(
            reconcile_active_lab_runner_handoffs().expect("read-side reconciliation"),
            0
        );
        let hydrating = record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "hydrating",
            Some("/runner/workspace/homeboy"),
            None,
            None,
            Some(&plan),
        )
        .expect("long setup remains controller-owned");
        assert_eq!(hydrating.state, AgentTaskRunState::Queued);
        assert_eq!(hydrating.metadata["phase"], "hydrating");

        let cancelled = cancel_run(run_id, Some("controller cancelled during setup"))
            .expect("terminalize setup record");
        let after_terminal_phase = record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "provider_dispatch",
            Some("/runner/workspace/homeboy"),
            None,
            None,
            Some(&plan),
        )
        .expect("terminal phase write is a no-op");
        assert_eq!(after_terminal_phase, cancelled);
        assert!(after_terminal_phase.lab_handoff.is_none());
        assert!(after_terminal_phase
            .metadata
            .get("handoff_acceptance")
            .is_none());
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
        apply_runner_job_terminal_state(
            &mut record,
            homeboy_core::api_jobs::JobStatus::Succeeded,
            &[],
        );
        store::write_record(&record).expect("terminal record");
        std::fs::remove_file(&record.plan_path).expect("remove controller plan");

        let attached = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-late-artifact",
            runner_id: "homeboy-lab",
            runner_job_id: "job-original",
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
fn queued_runner_child_reports_fifo_capacity_ownership_before_its_claim() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-queued-capacity",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("queued proxy");
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = homeboy_core::api_jobs::JobStatus::Queued;
        snapshot.job.target_runner_id = Some("homeboy-lab".to_string());
        snapshot.events.clear();

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("queued reconciliation");

        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(record.metadata["phase"], "waiting_for_capacity");
        assert_eq!(record.metadata["provider_state"], "queued");
        assert_eq!(
            record.metadata["runner_queue"],
            json!({
                "owner_runner_id": "homeboy-lab",
                "ordering": "fifo",
                "dispatch_eligibility": "runner_capacity_lease",
                "state": "waiting_for_capacity",
            })
        );

        snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("claim reconciliation");
        assert_eq!(record.metadata["phase"], "executing");
        assert_eq!(record.metadata["runner_queue"]["state"], "claimed");
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
fn transport_only_reconciliation_stays_pending_until_foreground_projection_or_inner_aggregate() {
    with_isolated_home(|_| {
        let run_id = "foreground-transport-only";
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let inner_patch = b"late aggregate patch";
        let inner_patch_sha256 = format!("{:x}", sha2::Sha256::digest(inner_patch));
        let finalized_dir = homeboy_core::paths::artifact_root()
            .expect("artifact root")
            .join("executor-finalized");
        std::fs::create_dir_all(&finalized_dir).expect("controller finalized artifact directory");
        std::fs::write(finalized_dir.join("inner-patch"), inner_patch)
            .expect("controller finalized artifact bytes");
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id,
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
        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(record.metadata["phase"], "awaiting_runner_synchronization");
        assert_eq!(
            record.metadata["runner_result_synchronization"]["state"],
            "pending"
        );
        assert!(artifacts(&record.run_id)
            .expect("transport-only artifacts")
            .artifacts
            .is_empty());

        assert!(
            project_terminal_runner_result(&record.run_id, &transport_only)
                .expect("foreground transport result projects the explicit run")
        );
        assert_eq!(record.state, AgentTaskRunState::Running);
        let projected = status(&record.run_id).expect("foreground terminal projection");
        assert_eq!(projected.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            projected.metadata["runner_result_synchronization"]["state"],
            "projected"
        );
        record = projected;

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
            url: Some(format!(
                "homeboy://agent-task/run/{run_id}/artifacts#task=task-a&artifact=inner-patch"
            )),
            mime: Some("text/x-diff".to_string()),
            size_bytes: Some(inner_patch.len() as u64),
            sha256: Some(inner_patch_sha256.clone()),
            metadata: Value::Null,
        });
        let mut lifecycle_snapshot = terminal_child_snapshot(&aggregate);
        let identity = &mut lifecycle_snapshot.events[0]
            .data
            .as_mut()
            .expect("lifecycle event data")["identity"];
        identity["run_id"] = json!(run_id);
        identity["persisted_run_id"] = json!(run_id);
        reconcile_runner_job_snapshot(&mut record, &lifecycle_snapshot)
            .expect("late inner lifecycle evidence is adopted");

        let artifacts = artifacts(&record.run_id).expect("controller-visible artifacts");
        assert_eq!(artifacts.artifacts.len(), 1);
        assert_eq!(artifacts.artifacts[0].id, "inner-patch");
        assert_eq!(
            artifacts.artifacts[0].size_bytes,
            Some(inner_patch.len() as u64)
        );
        assert_eq!(
            artifacts.artifacts[0].sha256.as_deref(),
            Some(inner_patch_sha256.as_str())
        );
        assert_eq!(
            store::read_aggregate(&record.run_id)
                .expect("persisted inner aggregate")
                .outcomes[0]
                .artifacts[0]
                .sha256
                .as_deref(),
            Some(inner_patch_sha256.as_str())
        );
    });
}

#[test]
fn foreground_terminal_daemon_projection_finishes_success_and_failure_runs_once() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, daemon_status, expected_run, expected_task, expected_execution_status) in [
            (
                "foreground-daemon-success",
                homeboy_core::api_jobs::JobStatus::Succeeded,
                AgentTaskRunState::Succeeded,
                AgentTaskState::Succeeded,
                "succeeded",
            ),
            (
                "foreground-daemon-failure",
                homeboy_core::api_jobs::JobStatus::Failed,
                AgentTaskRunState::Failed,
                AgentTaskState::Failed,
                "failed",
            ),
        ] {
            record_detached_lab_run(DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            })
            .expect("accepted detached handoff");
            let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
            snapshot.job.status = daemon_status;
            snapshot.events.clear();

            assert!(project_terminal_runner_result(run_id, &snapshot)
                .expect("foreground daemon result is projected"));
            assert!(!project_terminal_runner_result(run_id, &snapshot)
                .expect("repeated terminal result is idempotent"));

            let projected = status(run_id).expect("terminal durable run");
            assert_eq!(projected.state, expected_run);
            assert_eq!(projected.tasks[0].state, expected_task);
            assert_eq!(projected.lifecycle.execution.state, expected_run.into());
            assert!(projected.lifecycle.execution.finished_at.is_some());
            assert_eq!(
                projected.metadata["runner_job_status"],
                json!(daemon_status)
            );
            assert_eq!(
                projected.metadata["runner_execution_record"]["status"],
                expected_execution_status
            );
            assert_eq!(projected.metadata["runner_handoff"]["state"], "terminal");
            assert_eq!(
                projected.metadata["runner_result_synchronization"]["state"],
                "projected"
            );
        }
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
fn detached_lab_handoff_upgrades_existing_observation_record() {
    with_isolated_home(|_| {
        let store = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        store
            .upsert_imported_run(&homeboy_core::observation::RunRecord {
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
        let session_path = homeboy_core::paths::runner_session_file("local").expect("session path");
        std::fs::create_dir_all(session_path.parent().expect("session parent"))
            .expect("create session parent");
        std::fs::write(
            session_path,
            serde_json::to_string(&session).expect("session JSON"),
        )
        .expect("write session");
        struct FakeRunnerEvidence;
        impl homeboy_core::observation::runs_service::RunnerEvidenceProvider for FakeRunnerEvidence {
            fn mirror_connected_runner_run(
                &self,
                _: &str,
            ) -> Result<Option<homeboy_core::observation::RunRecord>> {
                Ok(None)
            }
            fn statuses(
                &self,
            ) -> Vec<homeboy_core::observation::runs_service::RunnerConnectionInfo> {
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
            ) -> Result<(
                homeboy_core::api_jobs::Job,
                Vec<homeboy_core::api_jobs::JobEvent>,
            )> {
                unreachable!()
            }
            fn refresh_mirrored_daemon_evidence(
                &self,
                _: &str,
            ) -> Result<Option<Vec<homeboy_core::observation::RunRecord>>> {
                Ok(None)
            }
            fn mirrored_runner_job_identity(
                &self,
                _: &homeboy_core::observation::RunRecord,
            ) -> Option<(String, String)> {
                None
            }
            fn download_remote_artifact(
                &self,
                path: &str,
                output: Option<std::path::PathBuf>,
            ) -> Result<homeboy_core::observation::runs_service::RemoteArtifactDownloadInfo>
            {
                assert_eq!(path, "runner-artifact://local/detached-run/patch");
                let output_path = output.unwrap_or_else(|| {
                    homeboy_core::paths::artifact_root()
                        .expect("artifact root")
                        .join("fake-runner-patch")
                });
                std::fs::write(&output_path, b"patch bytes").expect("write fake runner bytes");
                Ok(
                    homeboy_core::observation::runs_service::RemoteArtifactDownloadInfo {
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
        homeboy_core::observation::runs_service::register_runner_evidence_provider(Box::new(
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
        let observation = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("observation store");
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
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let artifact = homeboy_core::observation::runs_service::resolve_artifact_for_run(
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
fn record_health_reconciles_plan_backed_missing_metadata_idempotently() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("repairable-metadata")).expect("submitted");
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
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
fn tasks_for_aggregate_projects_dynamically_selected_model_when_plan_declares_none() {
    // #9404: provider rotation can select a concrete model dynamically, so the
    // plan task has model=None but the authoritative terminal outcome records
    // the model actually used. The reconciled task must carry that selected
    // model so canonical provider_runtime metadata is not null and PR
    // finalization is not spuriously rejected.
    let plan = test_plan();
    assert!(
        plan.tasks[0].executor.model.is_none(),
        "fixture plan must declare no model to exercise dynamic selection"
    );

    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].metadata = json!({ "model": "openai/gpt-5.6-terra" });

    let tasks = crate::agent_task_lifecycle::conversion::tasks_for_aggregate(&plan, &aggregate);

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].model.as_deref(), Some("openai/gpt-5.6-terra"));
}

#[test]
fn tasks_for_aggregate_preserves_declared_plan_model_over_outcome_model() {
    // A declared plan model is authoritative and must never be overridden by a
    // divergent outcome model; declared-model behavior stays unchanged.
    let mut plan = test_plan();
    plan.tasks[0].executor.model = Some("declared/plan-model".to_string());

    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].metadata = json!({ "model": "other/outcome-model" });

    let tasks = crate::agent_task_lifecycle::conversion::tasks_for_aggregate(&plan, &aggregate);

    assert_eq!(tasks[0].model.as_deref(), Some("declared/plan-model"));
}
