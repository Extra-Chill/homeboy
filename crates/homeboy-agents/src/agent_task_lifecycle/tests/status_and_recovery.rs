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
fn cook_alias_status_projects_active_adoption_from_earlier_attempt() {
    with_isolated_home(|_| {
        let cook_id = "cook-issue-9168-active";
        let earlier = submit_plan(&test_plan(), Some("adoption-earlier")).expect("earlier run");
        record_cook_attempt(cook_id, 1, &earlier.run_id).expect("index earlier run");
        start_candidate_adoption(
            &earlier.run_id,
            "1111111111111111111111111111111111111111",
            "openai/gpt-5.6-sol",
            "cargo test",
        )
        .expect("start earlier adoption");

        let latest = submit_plan(&test_plan(), Some("adoption-latest")).expect("latest run");
        record_cook_attempt(cook_id, 2, &latest.run_id).expect("index latest run");

        let unrelated =
            submit_plan(&test_plan(), Some("adoption-unrelated")).expect("unrelated run");
        start_candidate_adoption(
            &unrelated.run_id,
            "9999999999999999999999999999999999999999",
            "openai/gpt-5.6-sol",
            "cargo test",
        )
        .expect("start unrelated adoption");

        let projected = status(cook_id).expect("Cook alias status");
        assert_eq!(projected.run_id, latest.run_id);
        assert_eq!(projected.state, latest.state);
        assert_eq!(
            projected.adoption_run_id.as_deref(),
            Some(earlier.run_id.as_str())
        );
        let adoption = projected.candidate_adoption.expect("projected adoption");
        assert_eq!(adoption.state, "verification_running");
        assert_eq!(
            adoption.candidate_sha,
            "1111111111111111111111111111111111111111"
        );
    });
}

#[test]
fn cook_alias_status_has_no_adoption_projection_without_indexed_adoption() {
    with_isolated_home(|_| {
        let cook_id = "cook-issue-9168-none";
        let first = submit_plan(&test_plan(), Some("no-adoption-first")).expect("first run");
        record_cook_attempt(cook_id, 1, &first.run_id).expect("index first run");
        let latest = submit_plan(&test_plan(), Some("no-adoption-latest")).expect("latest run");
        record_cook_attempt(cook_id, 2, &latest.run_id).expect("index latest run");

        let projected = status(cook_id).expect("Cook alias status");
        assert_eq!(projected.run_id, latest.run_id);
        assert!(projected.adoption_run_id.is_none());
        assert!(projected.candidate_adoption.is_none());
    });
}

#[test]
fn cook_alias_status_selects_latest_terminal_adoption_then_index_order() {
    with_isolated_home(|_| {
        let cook_id = "cook-issue-9168-terminal";
        let mut runs = Vec::new();
        for attempt in 1..=3 {
            let run = submit_plan(&test_plan(), Some(&format!("terminal-adoption-{attempt}")))
                .expect("terminal run");
            record_cook_attempt(cook_id, attempt, &run.run_id).expect("index terminal run");
            start_candidate_adoption(
                &run.run_id,
                &format!("{attempt:040}"),
                "openai/gpt-5.6-sol",
                "cargo test",
            )
            .expect("start terminal adoption");
            finish_candidate_adoption(
                &run.run_id,
                (attempt != 1).then(|| format!("attempt {attempt} failed")),
            )
            .expect("finish terminal adoption");
            runs.push(run);
        }
        for (run, timestamp) in runs.iter().zip([
            "2026-07-20T12:00:03+00:00",
            "2026-07-20T12:00:01+00:00",
            "2026-07-20T12:00:03+00:00",
        ]) {
            rewrite_record_for_test(&run.run_id, |record| {
                record
                    .candidate_adoption
                    .as_mut()
                    .expect("terminal adoption")
                    .updated_at = timestamp.to_string();
            })
            .expect("set deterministic adoption timestamp");
        }

        let projected = status(cook_id).expect("Cook alias status");
        assert_eq!(projected.run_id, runs[2].run_id);
        assert_eq!(
            projected.adoption_run_id.as_deref(),
            Some(runs[2].run_id.as_str())
        );
        let adoption = projected.candidate_adoption.expect("terminal projection");
        assert_eq!(adoption.state, "failed");
        assert_eq!(adoption.candidate_sha, format!("{:040}", 3));
    });
}

#[test]
fn exact_run_id_status_keeps_its_own_adoption_without_alias_projection() {
    with_isolated_home(|_| {
        let cook_id = "cook-issue-9168-exact";
        let earlier = submit_plan(&test_plan(), Some("exact-earlier")).expect("earlier run");
        record_cook_attempt(cook_id, 1, &earlier.run_id).expect("index earlier run");
        start_candidate_adoption(
            &earlier.run_id,
            "2222222222222222222222222222222222222222",
            "openai/gpt-5.6-sol",
            "cargo test",
        )
        .expect("start earlier adoption");
        let latest = submit_plan(&test_plan(), Some("exact-latest")).expect("latest run");
        record_cook_attempt(cook_id, 2, &latest.run_id).expect("index latest run");

        let exact_earlier = status(&earlier.run_id).expect("exact earlier status");
        assert_eq!(exact_earlier.run_id, earlier.run_id);
        assert!(exact_earlier.adoption_run_id.is_none());
        assert_eq!(
            exact_earlier
                .candidate_adoption
                .expect("own adoption")
                .candidate_sha,
            "2222222222222222222222222222222222222222"
        );

        let exact_latest = status(&latest.run_id).expect("exact latest status");
        assert_eq!(exact_latest.run_id, latest.run_id);
        assert!(exact_latest.adoption_run_id.is_none());
        assert!(exact_latest.candidate_adoption.is_none());
    });
}

#[test]
fn candidate_adoption_status_persists_running_stale_resume_and_completion() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("adoption-progress")).expect("submit");
        let candidate = "a3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";

        start_candidate_adoption(
            &record.run_id,
            candidate,
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .expect("claim before verifier starts");
        let running = status(&record.run_id).expect("status exposes active adoption");
        let adoption = running
            .candidate_adoption
            .expect("durable adoption attempt");
        assert_eq!(adoption.state, "verification_running");
        assert_eq!(adoption.candidate_sha, candidate);
        assert_eq!(adoption.active_gate, "cargo test");

        let duplicate = start_candidate_adoption(
            &record.run_id,
            candidate,
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .expect_err("live duplicate is rejected");
        assert_eq!(duplicate.details["field"], "candidate_ref");

        rewrite_record_for_test(&record.run_id, |record| {
            record
                .candidate_adoption
                .as_mut()
                .expect("attempt")
                .owner_pid = u32::MAX;
        })
        .expect("make owner stale without sleeping");
        let interrupted = status(&record.run_id).expect("status reconciles stale owner");
        assert_eq!(
            interrupted.candidate_adoption.expect("attempt").state,
            "interrupted"
        );

        for (other_candidate, other_model) in [
            (
                "b3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
                "openai/gpt-5.6-terra",
            ),
            (candidate, "openai/gpt-5.6-sol"),
        ] {
            let conflict = start_candidate_adoption(
                &record.run_id,
                other_candidate,
                other_model,
                "cargo test",
            )
            .expect_err("interrupted attempt only resumes with exact candidate and model");
            assert_eq!(conflict.details["field"], "candidate_ref");
        }

        start_candidate_adoption(
            &record.run_id,
            candidate,
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .expect("same immutable candidate resumes");
        checkpoint_candidate_adoption(&record.run_id, "finalization", "finalize pull request")
            .expect("finalization checkpoint");
        let finalizing = status(&record.run_id).expect("finalization status");
        let adoption = finalizing.candidate_adoption.expect("attempt");
        assert_eq!(adoption.phase, "finalization");
        assert_eq!(adoption.active_gate, "finalize pull request");
        finish_candidate_adoption(&record.run_id, None).expect("terminal completion");
        let completed = status(&record.run_id).expect("completed status");
        let adoption = completed
            .candidate_adoption
            .expect("terminal attempt retained");
        assert_eq!(adoption.state, "completed");
        assert_eq!(adoption.resume_count, 1);
        assert!(adoption.completed_at.is_some());
        assert!(start_candidate_adoption(
            &record.run_id,
            candidate,
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .is_err());
        start_candidate_adoption_with_rerun_policy(
            &record.run_id,
            candidate,
            "openai/gpt-5.6-terra",
            "cargo test",
            true,
        )
        .expect("explicit recipe policy permits a completed gate rerun");
    });
}

#[test]
fn interrupted_candidate_adoption_can_be_explicitly_replaced_with_audit_history() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("adoption-replacement")).expect("submit");
        let original = "a3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";
        let replacement = "b3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";
        start_candidate_adoption(
            &record.run_id,
            original,
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .expect("start original adoption");
        assert!(start_candidate_adoption_with_policy(
            &record.run_id,
            replacement,
            "openai/gpt-5.6-sol",
            "cargo test",
            false,
            true,
        )
        .is_err());

        rewrite_record_for_test(&record.run_id, |record| {
            record
                .candidate_adoption
                .as_mut()
                .expect("original adoption")
                .owner_pid = u32::MAX;
        })
        .unwrap();
        status(&record.run_id).expect("reconcile stale owner");
        let replaced = start_candidate_adoption_with_policy(
            &record.run_id,
            replacement,
            "openai/gpt-5.6-sol",
            "cargo test",
            false,
            true,
        )
        .expect("explicitly replace interrupted adoption");

        let current = replaced.candidate_adoption.expect("replacement adoption");
        assert_eq!(current.candidate_sha, replacement);
        assert_eq!(current.ai_model, "openai/gpt-5.6-sol");
        assert_eq!(
            replaced.metadata["candidate_adoption_replacements"][0]["candidate_sha"],
            original
        );
        assert_eq!(
            replaced.metadata["candidate_adoption_replacements"][0]["state"],
            "interrupted"
        );
    });
}

#[test]
fn candidate_adoption_gate_heartbeats_are_durable() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("adoption-gate-supervision")).expect("submit");
        start_candidate_adoption(
            &record.run_id,
            "c3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
            "openai/gpt-5.6-terra",
            "cargo test",
        )
        .expect("start adoption");
        start_candidate_adoption_gate(&record.run_id, "cargo test", u32::MAX, 1800)
            .expect("persist gate identity before child work");
        heartbeat_candidate_adoption_gate(&record.run_id, "running output tail")
            .expect("persist periodic gate heartbeat");
        let running = status(&record.run_id).expect("read running adoption");
        let adoption = running.candidate_adoption.expect("active adoption");
        assert_eq!(adoption.phase, "gate_running");
        assert_eq!(adoption.gate_process_group, Some(u32::MAX));
        assert_eq!(adoption.gate_timeout_seconds, Some(1800));
        assert_eq!(adoption.gate_output_tail, "running output tail");
    });
}

#[cfg(unix)]
#[test]
fn candidate_adoption_reconciles_and_cancels_an_orphaned_gate_group() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("adoption-orphaned-gate")).expect("submit");
        start_candidate_adoption(
            &record.run_id,
            "d3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
            "openai/gpt-5.6-terra",
            "sleep 30",
        )
        .expect("start adoption");
        let mut command = std::process::Command::new("sh");
        command.args(["-lc", "sleep 30"]);
        homeboy_core::engine::command::isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn isolated fake gate");
        start_candidate_adoption_gate(&record.run_id, "sleep 30", child.id(), 1800)
            .expect("persist gate identity");
        rewrite_record_for_test(&record.run_id, |record| {
            record
                .candidate_adoption
                .as_mut()
                .expect("adoption")
                .owner_pid = u32::MAX;
        })
        .expect("simulate controller interruption");

        let interrupted = status(&record.run_id).expect("reconcile orphaned gate");
        assert_eq!(
            interrupted.candidate_adoption.expect("adoption").phase,
            "gate_orphaned"
        );
        assert!(start_candidate_adoption(
            &record.run_id,
            "d3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
            "openai/gpt-5.6-terra",
            "sleep 30",
        )
        .is_err());
        let cancelled = cancel_run(&record.run_id, Some("recover orphaned gate"))
            .expect("cancel orphaned gate");
        assert_eq!(
            cancelled.candidate_adoption.expect("adoption").state,
            "cancelled"
        );
        assert!(
            !homeboy_core::process::isolated_process_group_is_running(child.id())
                .expect("inspect terminated gate group")
        );
        let _ = child.wait();
    });
}

#[cfg(unix)]
#[test]
fn candidate_adoption_cancellation_persists_request_before_group_termination() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("adoption-cancel-race")).expect("submit");
        start_candidate_adoption(
            &record.run_id,
            "e3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
            "openai/gpt-5.6-terra",
            "sleep 30",
        )
        .expect("start adoption");
        let mut command = std::process::Command::new("sh");
        command.args(["-lc", "trap '' TERM; while :; do :; done"]);
        homeboy_core::engine::command::isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn isolated gate");
        start_candidate_adoption_gate(&record.run_id, "sleep 30", child.id(), 1800)
            .expect("persist gate identity");

        let run_id = record.run_id.clone();
        let cancellation = std::thread::spawn(move || cancel_run(&run_id, Some("operator cancel")));
        let observed_request = (0..100).any(|_| {
            let state = status(&record.run_id)
                .expect("read adoption")
                .candidate_adoption
                .expect("adoption")
                .state;
            if state == "cancel_requested" {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            false
        });
        let cancelled = cancellation
            .join()
            .expect("join cancellation")
            .expect("cancelled");
        assert!(observed_request, "cancellation request was never durable");
        assert_eq!(
            cancelled.candidate_adoption.expect("adoption").state,
            "cancelled"
        );
        let _ = child.wait();
    });
}

#[cfg(unix)]
#[test]
fn artifact_recovery_rejects_wrong_hash_and_identity_without_record_mutation() {
    with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let artifact = temporary.path().join("exact-homeboy");
        let digest = fake_controller_artifact(&artifact, &identity, "exact artifact");
        let legacy = temporary.path().join("legacy-homeboy");
        std::fs::write(&legacy, b"corrupted legacy bytes").expect("write corrupted legacy pin");
        let record = submit_plan(&test_plan(), Some("recover-reject-artifact")).expect("submit");

        rewrite_record_for_test(&record.run_id, |record| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
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
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
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

#[test]
fn detached_lab_handoff_persists_inspectable_running_record() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        // A detached running record reconciles against runner connectivity. Install
        // a connected runner so the record is not flagged `runner_disconnected`
        // stale by the no-op default (#8964). The guard restores the default on drop.
        let _runner =
            RunnerContinuationTestGuard::install(Box::new(super::ConnectedRunnerProvider));
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
fn detached_cook_intent_reconciliation_converges_both_crash_windows_without_secret_leakage() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let store = JobStore::default();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let fail_after_accept_once = Arc::new(Mutex::new(false));
        // Scope the provider to this test so it cannot leak into later tests and
        // make lifecycle results order-dependent (#8964).
        let _runner = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
            store: store.clone(),
            submitted: Arc::clone(&submitted),
            lookups: Arc::clone(&lookups),
            fail_after_accept_once: Arc::clone(&fail_after_accept_once),
        }));
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];

        for (run_id, post_accept_fault) in
            [("fault-after-intent", false), ("fault-after-post", true)]
        {
            record_lab_offload_planned(LabOffloadProxyPlan {
                run_id,
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: None,
            })
            .expect("record Lab admission");
            record_lab_offload_submission_intent(
                run_id,
                "homeboy-lab",
                "/runner/workspace/homeboy",
                &command,
                &["HOMEBOY_TEST_REVERSE_SECRET".to_string()],
            )
            .expect("persist redacted pre-submit intent");
            record_lab_offload_submission_request(run_id, &replay_request(run_id, &command))
                .expect("persist exact request before post");

            if post_accept_fault {
                *fail_after_accept_once.lock().expect("fault flag") = true;
                assert!(
                    !reconcile_pending_runner_submission_intent(run_id).expect("fault is retained")
                );
            }
            assert!(reconcile_pending_runner_submission_intent(run_id).expect("replay intent"));
            assert!(!reconcile_pending_runner_submission_intent(run_id).expect("duplicate wake"));
            let record = status(run_id).expect("accepted lifecycle");
            assert_eq!(
                record.metadata["runner_submission_intent"]["state"],
                "accepted"
            );
            assert!(!serde_json::to_string(&record)
                .expect("record JSON")
                .contains("secret-value"));
        }

        let submitted = submitted.lock().expect("submission log");
        assert_eq!(
            submitted.len(),
            3,
            "post-accept replay reuses the broker submission key"
        );
        assert_eq!(submitted[1], submitted[2]);
        let persisted = serde_json::to_string(&store.get(submitted[0]).expect("broker job"))
            .expect("broker JSON");
        assert!(!persisted.contains("secret-value"));
        assert!(lookups.lock().expect("lookup log").is_empty());
    });
}

#[test]
fn cancelled_or_expired_pending_handoff_never_submits_new_runner_work() {
    with_isolated_home(|_| {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        register_runner_continuation_provider(Box::new(IntentReplayProvider {
            store: JobStore::default(),
            submitted: Arc::clone(&submitted),
            lookups: Arc::clone(&lookups),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];

        for run_id in ["cancel-before-admission", "expire-before-admission"] {
            record_lab_offload_planned(LabOffloadProxyPlan {
                run_id,
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: None,
            })
            .expect("record Lab admission");
            record_lab_offload_submission_intent(
                run_id,
                "homeboy-lab",
                "/runner/workspace/homeboy",
                &command,
                &[],
            )
            .expect("persist intent");
        }

        cancel_run("cancel-before-admission", Some("operator cancelled"))
            .expect("cancel before daemon acceptance");
        record_lab_offload_submission_request(
            "expire-before-admission",
            &replay_request("expire-before-admission", &command),
        )
        .expect("persist complete pending request");
        rewrite_record_for_test("expire-before-admission", |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("expire handoff");

        assert!(
            !reconcile_pending_runner_submission_intent("cancel-before-admission")
                .expect("cancelled handoff is not submitted")
        );
        assert!(
            !reconcile_pending_runner_submission_intent("expire-before-admission")
                .expect("expired handoff is not submitted")
        );
        assert!(submitted.lock().expect("submission log").is_empty());
        assert!(lookups.lock().expect("lookup log").is_empty());
    });
}

#[test]
fn preparing_crash_never_submits_or_queries_the_runner() {
    with_isolated_home(|_| {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        register_runner_continuation_provider(Box::new(IntentReplayProvider {
            store: JobStore::default(),
            submitted: Arc::clone(&submitted),
            lookups: Arc::clone(&lookups),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "preparing-crash",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned handoff");
        record_lab_offload_submission_intent(
            "preparing-crash",
            "homeboy-lab",
            "/runner/workspace/homeboy",
            &command,
            &[],
        )
        .expect("preparing intent");

        assert!(!reconcile_pending_runner_submission_intent("preparing-crash").expect("no replay"));
        assert_eq!(
            status("preparing-crash").expect("status").state,
            AgentTaskRunState::Queued
        );
        assert!(submitted.lock().expect("submitted").is_empty());
        assert!(lookups.lock().expect("lookups").is_empty());
    });
}

#[test]
fn expired_or_cancelled_pending_submission_binds_and_cancels_the_accepted_job() {
    with_isolated_home(|_| {
        let store = JobStore::default();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        register_runner_continuation_provider(Box::new(IntentReplayProvider {
            store: store.clone(),
            submitted: Arc::clone(&submitted),
            lookups: Arc::clone(&lookups),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];

        for run_id in ["accepted-then-expired", "accepted-then-cancelled"] {
            record_lab_offload_planned(LabOffloadProxyPlan {
                run_id,
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: None,
            })
            .expect("planned handoff");
            record_lab_offload_submission_intent(
                run_id,
                "homeboy-lab",
                "/runner/workspace/homeboy",
                &command,
                &[],
            )
            .expect("preparing intent");
            let request = replay_request(run_id, &command);
            record_lab_offload_submission_request(run_id, &request).expect("pending request");
            let job = store
                .submit_remote_runner_job(request)
                .expect("accepted broker job");

            if run_id == "accepted-then-expired" {
                rewrite_record_for_test(run_id, |record| {
                    record
                        .lab_handoff
                        .as_mut()
                        .expect("handoff")
                        .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
                })
                .expect("expire deadline");
                let record = status(run_id).expect("late acceptance reconciliation");
                let job_id = job.id.to_string();
                assert_eq!(record.runner_job_id(), Some(job_id.as_str()));
                assert_eq!(record.state, AgentTaskRunState::Running);
            } else {
                let cancellation_store = store.clone();
                let _guard = crate::agent_task_lifecycle::cancellation::test_cancel_hook::install(
                    Box::new({
                        let expected_job_id = job.id.to_string();
                        move |runner_id, job_id, durable_run_id| {
                            assert_eq!(runner_id, "homeboy-lab");
                            assert_eq!(job_id, expected_job_id);
                            assert_eq!(durable_run_id, "accepted-then-cancelled");
                            Ok((cancellation_store.get(job.id).expect("job"), Vec::new()))
                        }
                    }),
                );
                let record =
                    cancel_run(run_id, Some("operator cancellation")).expect("cancel bound job");
                assert_eq!(record.state, AgentTaskRunState::Cancelled);
                let job_id = job.id.to_string();
                assert_eq!(record.runner_job_id(), Some(job_id.as_str()));
            }
        }
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "absent-after-deadline",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("planned absent handoff");
        record_lab_offload_submission_intent(
            "absent-after-deadline",
            "homeboy-lab",
            "/runner/workspace/homeboy",
            &command,
            &[],
        )
        .expect("preparing absent handoff");
        record_lab_offload_submission_request(
            "absent-after-deadline",
            &replay_request("absent-after-deadline", &command),
        )
        .expect("pending absent handoff");
        rewrite_record_for_test("absent-after-deadline", |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("expire absent handoff");
        let absent = status("absent-after-deadline").expect("absent reconciliation");
        assert_eq!(absent.state, AgentTaskRunState::Cancelled);
        assert!(absent.runner_job_id().is_none());
        assert!(submitted.lock().expect("submitted").is_empty());
        let lookups = lookups.lock().expect("lookups");
        for run_id in [
            "accepted-then-expired",
            "accepted-then-cancelled",
            "absent-after-deadline",
        ] {
            assert!(lookups.contains(&format!("agent-task:v1:homeboy-lab:{run_id}")));
        }
    });
}

#[test]
fn retryable_workspace_metadata_transport_failure_builds_transient_outcome() {
    let plan = test_plan();
    let error = Error::new(
        ErrorCode::RunnerLabTransportFailure,
        "write runner workspace metadata failed during `workspace_metadata_write`",
        json!({
            "phase": "workspace_metadata_write",
            "command": "write Homeboy runner workspace metadata",
            "timeout_seconds": 30,
            "exit_code": -1,
            "stdout": "",
            "stderr": "Connection to 192.168.86.63 closed by remote host. client_loop: send disconnect: Broken pipe",
            "transport_close_reason": "Connection to 192.168.86.63 closed by remote host. client_loop: send disconnect: Broken pipe",
        }),
    )
    .with_retryable(true);
    let outcome = build_pre_execution_failure_outcome(
        "cook-8803-attempt-1",
        &plan.tasks[0],
        "lab_workspace_stage",
        &error,
    );

    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Transient)
    );
    assert_eq!(outcome.diagnostics[0].data["retryable"], true);
    assert_eq!(
        outcome.diagnostics[0].data["details"]["phase"],
        "workspace_metadata_write"
    );
    assert_eq!(outcome.outputs["retryable"], true);
    assert_eq!(
        outcome.outputs["details"]["transport_close_reason"],
        error.details["transport_close_reason"]
    );
    assert_eq!(outcome.metadata["retryable"], true);
    assert_eq!(outcome.metadata["provider_executions_consumed"], 0);
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
            data: Some(json!({
                "provider": "openai/gpt-5.6-terra",
                "phase": "implementing",
                "activity": "editing lifecycle projection"
            })),
        }]);
        store::write_record(&record).expect("persist mirrored event");

        let log = logs("live-runner-events").expect("live logs resolve");

        assert_eq!(log.events.len(), 1);
        assert!(log.events[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider started")));
        assert_eq!(
            log.events[0].provider.as_deref(),
            Some("openai/gpt-5.6-terra")
        );
        assert_eq!(log.events[0].phase.as_deref(), Some("implementing"));
        assert_eq!(
            log.events[0].activity.as_deref(),
            Some("editing lifecycle projection")
        );
        assert_eq!(log.events[0].heartbeat_at_ms, Some(42));
        assert!(log.raw_events.is_empty(), "raw transport is opt-in");

        let raw_log = logs_with_raw("live-runner-events", true).expect("raw logs resolve");
        assert_eq!(
            raw_log.events[0].metadata["provider"],
            "openai/gpt-5.6-terra"
        );
        assert_eq!(raw_log.raw_events.len(), 1);
        assert_eq!(
            raw_log.raw_events[0]["data"]["activity"],
            "editing lifecycle projection"
        );
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
        apply_runner_job_terminal_state(
            &mut record,
            homeboy_core::api_jobs::JobStatus::Succeeded,
            &[],
        );
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
        assert_eq!(error.details["field"], "lab_handoff");
    });
}

#[test]
fn accepted_handoff_waits_for_authoritative_aggregate_after_terminal_daemon_status() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, job_status) in [
            (
                "agent-task-remote-failure",
                homeboy_core::api_jobs::JobStatus::Failed,
            ),
            (
                "agent-task-remote-cancellation",
                homeboy_core::api_jobs::JobStatus::Cancelled,
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
                .expect("terminal daemon result records pending synchronization");

            assert_eq!(record.state, AgentTaskRunState::Running);
            assert_eq!(record.metadata["runner_job_status"], json!(job_status));
            assert_eq!(
                record.metadata["runner_result_synchronization"]["state"],
                "pending"
            );
            assert_eq!(record.metadata["phase"], "awaiting_runner_synchronization");
        }
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
        apply_runner_job_terminal_state(
            &mut record,
            homeboy_core::api_jobs::JobStatus::Succeeded,
            &[],
        );
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
                homeboy_core::runner_execution_envelope::RunnerExecutionRecord::in_flight(
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
            sha256: Some(homeboy_core::artifact_metadata::sha256_file(&patch).expect("sha")),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
        submit_plan(&plan, Some("projection-parity")).expect("submit");
        record_run_aggregate("projection-parity", &plan, &aggregate).expect("record aggregate");

        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let artifact = homeboy_core::observation::runs_service::resolve_artifact_for_run(
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
        let fetched = homeboy_core::observation::runs_service::copy_local_file_artifact(
            homeboy_core::observation::runs_service::resolve_artifact_for_run(
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
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let remote_alias = homeboy_core::observation::runs_service::resolve_artifact_for_run(
            &store,
            &submitted.run_id,
            "patch",
        )
        .expect("runner artifact alias remains available");
        assert_eq!(remote_alias.artifact_type, "remote_file");
        assert!(
            homeboy_core::execution_contract::is_remote_runner_artifact_path(&remote_alias.path)
        );
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
        assert_eq!(log.events[0].status, AgentTaskState::Succeeded);
        assert_eq!(artifact_report.schema, schemas::RUN_ARTIFACTS);
        assert_eq!(artifact_report.artifacts[0].id, "patch");
        assert_eq!(artifact_report.evidence_refs[0].kind, "transcript");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run_store-contract");
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
fn malformed_typed_pending_handoff_is_health_malformed_and_unreconciled() {
    with_isolated_home(|_| {
        submit_plan(&test_plan(), Some("malformed-typed-pending")).expect("submitted");
        rewrite_record_for_test("malformed-typed-pending", |record| {
            record.lab_handoff = Some(AgentTaskLabHandoff {
                state: AgentTaskLabHandoffState::Pending,
                authority: AgentTaskLabHandoffAuthority::Controller,
                runner_id: "homeboy-lab".to_string(),
                submission_key: None,
                payload_fingerprint: None,
                runner_job_id: None,
                submitted_at: Some("invalid".to_string()),
                acceptance_deadline_at: None,
                accepted_at: None,
                expired_at: None,
            });
        })
        .expect("malformed typed state stored");

        let health = record_health_summary().expect("health report");
        assert_eq!(health.malformed, 1);
        let report = reconcile_record_health(false).expect("quarantine malformed state");
        assert_eq!(report.quarantined, 1);
        assert_eq!(
            report.records[0].reason,
            AgentTaskRecordHealthReason::MalformedMetadata
        );
    });
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

        assert_eq!(log.schema, "homeboy/agent-task-run-log/v2");
        assert_eq!(log.events.len(), 2);
        assert_eq!(log.events[0].schema, schemas::EVENT);
        assert_eq!(log.events[0].run_id, "run-event-envelope");
        assert_eq!(log.events[0].task_id, "task-a");
        assert_eq!(log.events[0].sequence, 1);
        assert_eq!(log.events[0].status, AgentTaskState::Running);
        assert_eq!(log.events[1].message.as_deref(), Some("ok"));
        assert_eq!(log.events[1].artifact_refs.len(), 1);
        assert!(log.raw_events.is_empty());
    });
}

#[test]
fn set_run_state_stamps_finished_at_for_candidate_recoverable_terminal_runs() {
    // A run that finished with a recoverable candidate is terminal, so
    // set_run_state must stamp finished_at for it exactly as it does for the
    // other terminal states. Regression guard for the drift where the setter's
    // hand-listed terminal subset omitted CandidateRecoverable, leaving these
    // runs without a finished_at while the legacy-record migration path stamped
    // one.
    with_isolated_home(|_| {
        let record =
            submit_plan(&test_plan(), Some("candidate-recoverable-finished-at")).expect("submit");
        rewrite_record_for_test(&record.run_id, |record| {
            set_run_state(record, AgentTaskRunState::CandidateRecoverable);
            assert_eq!(record.state, AgentTaskRunState::CandidateRecoverable);
            assert!(
                record.lifecycle.execution.finished_at.is_some(),
                "a terminal CandidateRecoverable run must stamp finished_at"
            );
        })
        .expect("rewrite record");

        // And a non-terminal state must NOT stamp finished_at.
        rewrite_record_for_test(&record.run_id, |record| {
            record.lifecycle.execution.finished_at = None;
            set_run_state(record, AgentTaskRunState::Running);
            assert!(
                record.lifecycle.execution.finished_at.is_none(),
                "a non-terminal Running run must not stamp finished_at"
            );
        })
        .expect("rewrite record running");
    });
}
