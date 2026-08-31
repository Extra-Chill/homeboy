//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
#![cfg(test)]

use super::*;
use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcomeStatus};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use crate::agent_task_service::reconcile_stale_active_runs;
use homeboy_core::api_jobs::{Job, JobEvent, JobEventKind, JobStore, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::process::Stdio;
use std::sync::{Arc, Mutex};

/// The tests below drive the store-rooted lifecycle entry points. Resolving the
/// store once here keeps the ambient lookup in a single place instead of at
/// every call site, and lets the ambient wrappers be deleted (#7505).
fn test_lifecycle_store() -> AgentTaskLifecycleStore {
    AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store")
}

#[test]
fn candidate_recoverable_provider_projection_is_failed_not_timed_out() {
    assert_eq!(
        provider_runtime_state_for_task_state(Some(AgentTaskState::CandidateRecoverable)),
        ProviderRuntimeState::Failed
    );
}

#[test]
fn deferred_cleanup_missing_descriptor_is_persisted_as_actionable_diagnosis() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "deferred-cleanup-missing-descriptor";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submit run");
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.status = AgentTaskAggregateStatus::Failed;
    aggregate.outcomes[0].status = AgentTaskOutcomeStatus::Timeout;
    aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "deferred-cleanup".to_string(),
        kind: "cleanup_action".to_string(),
        name: Some("deferred-cleanup.json".to_string()),
        label: None,
        role: Some("cleanup_action".to_string()),
        semantic_key: None,
        path: Some(
            lifecycle_store
                .artifact_root()
                .join("agent-task/deferred-cleanup/missing.json")
                .display()
                .to_string(),
        ),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: None,
        sha256: None,
        metadata: json!({ "run_id": run_id, "task_id": "task-a", "attempt": 1 }),
    });
    record_run_aggregate_in_store(&lifecycle_store, run_id, &plan, &aggregate)
        .expect("persist timeout aggregate");

    assert!(
        reconcile_deferred_candidate_in_store(&lifecycle_store, run_id)
            .expect("reconcile missing descriptor")
    );
    let reconciled = lifecycle_store
        .read_aggregate(run_id)
        .expect("read aggregate");
    let diagnostic = reconciled.outcomes[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "agent_task.deferred_cleanup_descriptor_missing")
        .expect("missing descriptor diagnosis");
    assert_eq!(
        diagnostic.data["safe_next_action"],
        json!(format!("homeboy agent-task diagnose {run_id} --full"))
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Cook alias projection is a decision made *across* records — which
/// indexed attempt is latest, and which attempt owns the active adoption — so
/// the index, all three runs and the alias read have to come from one home. An
/// alias resolved against another installation's index would name attempts this
/// store has never heard of.
///
/// `start_candidate_adoption(a, b, c, d)` is exactly
/// `start_candidate_adoption_with_policy_in_store(store, a, b, c, d, false,
/// false)`: the ambient entry point delegates through
/// `start_candidate_adoption_with_rerun_policy` with both policy flags `false`.
#[test]
fn cook_alias_status_projects_active_adoption_from_earlier_attempt() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-issue-9168-active";
    let earlier = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-earlier", |_| Ok(json!({})))
        .expect("earlier run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, &earlier.run_id)
        .expect("index earlier run");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &earlier.run_id,
        "1111111111111111111111111111111111111111",
        "openai/gpt-5.6-sol",
        "cargo test",
        false,
        false,
    )
    .expect("start earlier adoption");

    let latest = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-latest", |_| Ok(json!({})))
        .expect("latest run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, &latest.run_id)
        .expect("index latest run");

    let unrelated = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-unrelated", |_| Ok(json!({})))
        .expect("unrelated run");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &unrelated.run_id,
        "9999999999999999999999999999999999999999",
        "openai/gpt-5.6-sol",
        "cargo test",
        false,
        false,
    )
    .expect("start unrelated adoption");

    let projected = reconcile_status_in_store(
        &lifecycle_store,
        cook_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("Cook alias status")
    .record;
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
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The assertion is an absence — no adoption was projected — which is
/// only meaningful if the index consulted is the one this test wrote.
#[test]
fn cook_alias_status_has_no_adoption_projection_without_indexed_adoption() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-issue-9168-none";
    let first = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "no-adoption-first", |_| Ok(json!({})))
        .expect("first run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, &first.run_id)
        .expect("index first run");
    let latest = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "no-adoption-latest", |_| Ok(json!({})))
        .expect("latest run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, &latest.run_id)
        .expect("index latest run");

    let projected = reconcile_status_in_store(
        &lifecycle_store,
        cook_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("Cook alias status")
    .record;
    assert_eq!(projected.run_id, latest.run_id);
    assert!(projected.adoption_run_id.is_none());
    assert!(projected.candidate_adoption.is_none());
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The alias projection is a *selection* across three indexed
/// attempts, decided by adoption timestamps this test plants by hand. The
/// index, all three records and the alias read have to come out of one
/// installation or the tie-break being asserted is not the tie-break that ran.
///
/// `start_candidate_adoption(a, b, c, d)` is exactly
/// `start_candidate_adoption_with_policy_in_store(store, a, b, c, d, false,
/// false)`: the ambient entry point delegates through
/// `start_candidate_adoption_with_rerun_policy` with both policy flags `false`.
#[test]
fn cook_alias_status_selects_latest_terminal_adoption_then_index_order() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-issue-9168-terminal";
    let mut runs = Vec::new();
    for attempt in 1..=3 {
        let run = lifecycle_store
            .submit_plan_with_runtime_admission(
                &test_plan(),
                &format!("terminal-adoption-{attempt}"),
                |_| Ok(json!({})),
            )
            .expect("terminal run");
        record_cook_attempt_in_store(&lifecycle_store, cook_id, attempt, &run.run_id)
            .expect("index terminal run");
        start_candidate_adoption_with_policy_in_store(
            &lifecycle_store,
            &run.run_id,
            &format!("{attempt:040}"),
            "openai/gpt-5.6-sol",
            "cargo test",
            false,
            false,
        )
        .expect("start terminal adoption");
        finish_candidate_adoption_in_store(
            &lifecycle_store,
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
        rewrite_record_for_test_in_store(&lifecycle_store, &run.run_id, |record| {
            record
                .candidate_adoption
                .as_mut()
                .expect("terminal adoption")
                .updated_at = timestamp.to_string();
        })
        .expect("set deterministic adoption timestamp");
    }

    let projected = reconcile_status_in_store(
        &lifecycle_store,
        cook_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("Cook alias status")
    .record;
    assert_eq!(projected.run_id, runs[2].run_id);
    assert_eq!(
        projected.adoption_run_id.as_deref(),
        Some(runs[2].run_id.as_str())
    );
    let adoption = projected.candidate_adoption.expect("terminal projection");
    assert_eq!(adoption.state, "failed");
    assert_eq!(adoption.candidate_sha, format!("{:040}", 3));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Both reads still go through the alias-resolving `reconcile_status_in_store`
/// (`exact: false`), exactly as the ambient `status` did — what this test
/// asserts is that naming a run id directly declines the alias projection, not
/// that a different entry point was used.
#[test]
fn exact_run_id_status_keeps_its_own_adoption_without_alias_projection() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-issue-9168-exact";
    let earlier = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "exact-earlier", |_| Ok(json!({})))
        .expect("earlier run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, &earlier.run_id)
        .expect("index earlier run");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &earlier.run_id,
        "2222222222222222222222222222222222222222",
        "openai/gpt-5.6-sol",
        "cargo test",
        false,
        false,
    )
    .expect("start earlier adoption");
    let latest = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "exact-latest", |_| Ok(json!({})))
        .expect("latest run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, &latest.run_id)
        .expect("index latest run");

    let exact_earlier = reconcile_status_in_store(
        &lifecycle_store,
        &earlier.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("exact earlier status")
    .record;
    assert_eq!(exact_earlier.run_id, earlier.run_id);
    assert!(exact_earlier.adoption_run_id.is_none());
    assert_eq!(
        exact_earlier
            .candidate_adoption
            .expect("own adoption")
            .candidate_sha,
        "2222222222222222222222222222222222222222"
    );

    let exact_latest = reconcile_status_in_store(
        &lifecycle_store,
        &latest.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("exact latest status")
    .record;
    assert_eq!(exact_latest.run_id, latest.run_id);
    assert!(exact_latest.adoption_run_id.is_none());
    assert!(exact_latest.candidate_adoption.is_none());
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). This is one adoption walked through claim, duplicate rejection,
/// staleness, resume, checkpoint and completion. Every step reads the state the
/// previous step committed — `resume_count == 1` at the end is a claim about
/// that whole sequence — so a single record in a single installation is the
/// only arrangement that makes the walk coherent.
///
/// `start_candidate_adoption(a, b, c, d)` is
/// `start_candidate_adoption_with_policy_in_store(store, a, b, c, d, false,
/// false)`, and `start_candidate_adoption_with_rerun_policy(a, b, c, d, rerun)`
/// is the same with `rerun_completed_gates = rerun, replace_interrupted =
/// false`: both ambient entry points delegate straight through
/// `start_candidate_adoption_with_policy`.
#[test]
fn candidate_adoption_status_persists_running_stale_resume_and_completion() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-progress", |_| Ok(json!({})))
        .expect("submit");
    let candidate = "a3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";

    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        candidate,
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .expect("claim before verifier starts");
    let running = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status exposes active adoption")
    .record;
    let adoption = running
        .candidate_adoption
        .expect("durable adoption attempt");
    assert_eq!(adoption.state, "verification_running");
    assert_eq!(adoption.candidate_sha, candidate);
    assert_eq!(adoption.active_gate, "cargo test");

    let duplicate = start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        candidate,
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .expect_err("live duplicate is rejected");
    assert_eq!(duplicate.details["field"], "candidate_ref");

    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record
            .candidate_adoption
            .as_mut()
            .expect("attempt")
            .owner_pid = u32::MAX;
    })
    .expect("make owner stale without sleeping");
    let interrupted = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status reconciles stale owner")
    .record;
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
        let conflict = start_candidate_adoption_with_policy_in_store(
            &lifecycle_store,
            &record.run_id,
            other_candidate,
            other_model,
            "cargo test",
            false,
            false,
        )
        .expect_err("interrupted attempt only resumes with exact candidate and model");
        assert_eq!(conflict.details["field"], "candidate_ref");
    }

    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        candidate,
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .expect("same immutable candidate resumes");
    checkpoint_candidate_adoption_in_store(
        &lifecycle_store,
        &record.run_id,
        "finalization",
        "finalize pull request",
    )
    .expect("finalization checkpoint");
    let finalizing = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("finalization status")
    .record;
    let adoption = finalizing.candidate_adoption.expect("attempt");
    assert_eq!(adoption.phase, "finalization");
    assert_eq!(adoption.active_gate, "finalize pull request");
    finish_candidate_adoption_in_store(&lifecycle_store, &record.run_id, None)
        .expect("terminal completion");
    let completed = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("completed status")
    .record;
    let adoption = completed
        .candidate_adoption
        .expect("terminal attempt retained");
    assert_eq!(adoption.state, "completed");
    assert_eq!(adoption.resume_count, 1);
    assert!(adoption.completed_at.is_some());
    assert!(start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        candidate,
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .is_err());
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        candidate,
        "openai/gpt-5.6-terra",
        "cargo test",
        true,
        false,
    )
    .expect("explicit recipe policy permits a completed gate rerun");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The audit history asserted at the end is the *original* adoption
/// preserved alongside its replacement, so the record that gets replaced and
/// the record the history is read from have to be the same one. The
/// intervening `reconcile_status_in_store` is what reconciles the falsified owner pid
/// into `interrupted`, and it must see the rewrite that preceded it.
#[test]
fn interrupted_candidate_adoption_can_be_explicitly_replaced_with_audit_history() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-replacement", |_| Ok(json!({})))
        .expect("submit");
    let original = "a3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";
    let replacement = "b3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1";
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        original,
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .expect("start original adoption");
    assert!(start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        replacement,
        "openai/gpt-5.6-sol",
        "cargo test",
        false,
        true,
    )
    .is_err());

    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record
            .candidate_adoption
            .as_mut()
            .expect("original adoption")
            .owner_pid = u32::MAX;
    })
    .unwrap();
    reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("reconcile stale owner");
    let replaced = start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
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
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Durability is the claim: the gate identity and the heartbeat are
/// written through siblings handed `lifecycle_store` and read back out of the
/// same home, so the supervision evidence asserted below survived a real
/// round-trip rather than being answered from another installation.
#[test]
fn public_candidate_adoption_gate_progress_is_durable() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-gate-supervision", |_| {
            Ok(json!({}))
        })
        .expect("submit");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        "c3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
        "openai/gpt-5.6-terra",
        "cargo test",
        false,
        false,
    )
    .expect("start adoption");
    start_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        "cargo test",
        u32::MAX,
        1800,
    )
    .expect("persist gate identity before child work");
    heartbeat_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        crate::agent_task_gate::AgentTaskGateVisibility::Visible,
        crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
        &crate::agent_task_gate::AgentTaskGateLiveStatus {
            visibility: crate::agent_task_gate::AgentTaskGateVisibility::Visible,
            reveal_policy: crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
            elapsed_ms: 42,
            last_progress_ms_ago: Some(7),
            progress: Some(homeboy_engine_primitives::command::CommandProgress {
                phase: "tests".to_string(),
                current: Some("case".to_string()),
            }),
            output_tail: "running output tail".to_string(),
        },
    )
    .expect("persist periodic gate heartbeat");
    let running = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("read running adoption")
    .record;
    let adoption = running.candidate_adoption.expect("active adoption");
    assert_eq!(adoption.phase, "gate_running");
    assert_eq!(adoption.gate_process_group, Some(u32::MAX));
    assert_eq!(adoption.gate_timeout_seconds, Some(1800));
    assert_eq!(adoption.gate_output_tail, "running output tail");
    assert_eq!(adoption.gate_elapsed_ms, Some(42));
    assert_eq!(adoption.gate_last_progress_ms_ago, Some(7));
    assert_eq!(
        adoption
            .gate_progress
            .expect("public progress")
            .current
            .as_deref(),
        Some("case")
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). This test asserts that secrets are *absent* from what was
/// persisted, so the record it serializes has to be the one the redacting
/// heartbeat actually wrote — reading an ambient home could report an absence
/// that proves nothing.
#[test]
fn private_candidate_adoption_gate_progress_is_redacted_before_persistence() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(
            &test_plan(),
            "private-adoption-gate-supervision",
            |_| Ok(json!({})),
        )
        .expect("submit");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        "d3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
        "openai/gpt-5.6-terra",
        "private gate",
        false,
        false,
    )
    .expect("start adoption");
    start_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        "private gate",
        u32::MAX,
        1800,
    )
    .expect("persist gate identity before child work");
    heartbeat_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        crate::agent_task_gate::AgentTaskGateVisibility::Private,
        crate::agent_task_gate::AgentTaskGateRevealPolicy::SummaryOnly,
        &crate::agent_task_gate::AgentTaskGateLiveStatus {
            visibility: crate::agent_task_gate::AgentTaskGateVisibility::Private,
            reveal_policy: crate::agent_task_gate::AgentTaskGateRevealPolicy::SummaryOnly,
            elapsed_ms: 42,
            last_progress_ms_ago: Some(7),
            progress: Some(homeboy_engine_primitives::command::CommandProgress {
                phase: "private-phase-secret".to_string(),
                current: Some("sha256:private-digest-123 count=42".to_string()),
            }),
            output_tail: "private output secret".to_string(),
        },
    )
    .expect("persist policy-filtered heartbeat");

    let adoption = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("read running adoption")
    .record
    .candidate_adoption
    .expect("active adoption");
    assert_eq!(adoption.gate_output_tail, "private gate output withheld");
    assert_eq!(adoption.gate_elapsed_ms, Some(0));
    assert!(adoption.gate_last_progress_ms_ago.is_none());
    assert!(adoption.gate_progress.is_none());
    let persisted = serde_json::to_string(&adoption).expect("adoption serializes");
    assert!(!persisted.contains("private-phase-secret"));
    assert!(!persisted.contains("private-digest-123"));
    assert!(!persisted.contains("private output secret"));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The alias resolves across two indexed attempts to project the
/// adoption owned by the *earlier* one, so the Cook index, both runs and the
/// alias read all have to name one installation. An alias resolved against
/// another home's index would name attempts this store has never heard of.
#[test]
fn cook_alias_status_projects_active_adoption_remediation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adoption-remediation-status";
    let source_run_id = "cook-adoption-remediation-status-attempt-1";
    let remediation_run_id = "cook-adoption-remediation-status-attempt-2";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, source_run_id, |_| Ok(json!({})))
        .expect("source run");
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, remediation_run_id, |_| Ok(json!({})))
        .expect("remediation run");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, source_run_id)
        .expect("source attempt");
    record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, remediation_run_id)
        .expect("remediation attempt");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        source_run_id,
        "candidate",
        "model",
        "cargo test",
        false,
        false,
    )
    .expect("active adoption");

    checkpoint_candidate_adoption_remediation_in_store(
        &lifecycle_store,
        source_run_id,
        remediation_run_id,
    )
    .expect("remediation checkpoint");

    let projected = reconcile_status_in_store(
        &lifecycle_store,
        cook_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("Cook alias status")
    .record;
    assert_eq!(projected.adoption_run_id.as_deref(), Some(source_run_id));
    let adoption = projected.candidate_adoption.expect("projected adoption");
    assert_eq!(adoption.state, "verification_running");
    assert_eq!(adoption.phase, "provider_remediation");
    assert_eq!(
        adoption.remediation_run_id.as_deref(),
        Some(remediation_run_id)
    );
    assert_eq!(
        adoption.remediation_status_command.as_deref(),
        Some("homeboy agent-task status cook-adoption-remediation-status-attempt-2")
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The gate process group is persisted on this record and the
/// cancellation reaps whatever group that record names, so planting the gate
/// identity in one home and cancelling from another would either reap nothing
/// or reap a group this test never spawned. The run is controller-local — no
/// runner id and no runner job id — so `cancel_run_in_store` never reaches
/// `classify_live_cancellation`'s runner-backed branch and the process-global
/// runner-continuation registry stays untouched. Same argument as
/// `candidate_adoption_cancellation_persists_request_before_group_termination`.
#[cfg(unix)]
#[test]
fn candidate_adoption_reconciles_and_cancels_an_orphaned_gate_group() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-orphaned-gate", |_| {
            Ok(json!({}))
        })
        .expect("submit");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        "d3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
        "openai/gpt-5.6-terra",
        "sleep 30",
        false,
        false,
    )
    .expect("start adoption");
    let mut command = std::process::Command::new("sh");
    command.args(["-lc", "sleep 30"]);
    homeboy_core::engine::command::isolate_process_tree(&mut command);
    let mut child = command.spawn().expect("spawn isolated fake gate");
    start_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        "sleep 30",
        child.id(),
        1800,
    )
    .expect("persist gate identity");
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record
            .candidate_adoption
            .as_mut()
            .expect("adoption")
            .owner_pid = u32::MAX;
    })
    .expect("simulate controller interruption");

    let interrupted = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("reconcile orphaned gate")
    .record;
    assert_eq!(
        interrupted.candidate_adoption.expect("adoption").phase,
        "gate_orphaned"
    );
    assert!(start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        "d3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
        "openai/gpt-5.6-terra",
        "sleep 30",
        false,
        false,
    )
    .is_err());
    let cancelled = cancel_run_in_store(
        &lifecycle_store,
        &record.run_id,
        Some("recover orphaned gate"),
    )
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
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The run is controller-local — no runner id and no runner job id —
/// so `cancel_run_in_store` never reaches `classify_live_cancellation`'s
/// runner-backed branch and the process-global runner-continuation registry
/// stays untouched. The spawned gate group is reaped exactly as before.
#[cfg(unix)]
#[test]
fn candidate_adoption_cancellation_persists_request_before_group_termination() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "adoption-cancel-race", |_| Ok(json!({})))
        .expect("submit");
    start_candidate_adoption_with_policy_in_store(
        &lifecycle_store,
        &record.run_id,
        "e3c3ad9c2b75f8b03d503f4a09f0e2c4d47b57e1",
        "openai/gpt-5.6-terra",
        "sleep 30",
        false,
        false,
    )
    .expect("start adoption");
    let mut command = std::process::Command::new("sh");
    command.args(["-lc", "trap '' TERM; while :; do :; done"]);
    homeboy_core::engine::command::isolate_process_tree(&mut command);
    let mut child = command.spawn().expect("spawn isolated gate");
    start_candidate_adoption_gate_in_store(
        &lifecycle_store,
        &record.run_id,
        "sleep 30",
        child.id(),
        1800,
    )
    .expect("persist gate identity");
    let reaper = std::thread::spawn(move || child.wait());

    let cancelled = cancel_run_in_store(&lifecycle_store, &record.run_id, Some("operator cancel"))
        .expect("cancelled");
    assert!(cancelled.metadata["candidate_adoption_cancel_requested_at"].is_string());
    assert_eq!(
        cancelled.candidate_adoption.expect("adoption").state,
        "cancelled"
    );
    reaper
        .join()
        .expect("join gate reaper")
        .expect("reap isolated gate");
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
        let before_hash = reconcile_status(&record.run_id).expect("record before wrong hash");
        let hash_error = recover_controller_runtime_in_store(
            &test_lifecycle_store(),
            &record.run_id,
            Some(&artifact),
            None,
        )
        .expect_err("wrong hash rejected");
        assert!(hash_error.message.contains("hash mismatch"));
        assert_eq!(
            reconcile_status(&record.run_id).expect("record after wrong hash"),
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
        let before_identity =
            reconcile_status(&record.run_id).expect("record before wrong identity");
        let identity_error = recover_controller_runtime_in_store(
            &test_lifecycle_store(),
            &record.run_id,
            Some(&artifact),
            None,
        )
        .expect_err("wrong identity rejected");
        assert!(identity_error.message.contains("build identity mismatch"));
        assert_eq!(
            reconcile_status(&record.run_id).expect("record after wrong identity"),
            before_identity
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The closing assertion is that the plan file was *not* rewritten, so
/// the execution read has to address the same `plan.json` this test replaced —
/// a rejection resolved from another home would leave the byte comparison
/// passing for the wrong reason.
#[test]
fn execution_budget_future_version_fails_closed_without_rewrite() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "future-budget", |_| Ok(json!({})))
        .expect("submitted");
    let mut raw: Value =
        serde_json::from_str(&std::fs::read_to_string(&record.plan_path).expect("persisted plan"))
            .expect("plan json");
    raw["options"]["execution_budget"]["version"] = json!(99);
    let future = serde_json::to_string_pretty(&raw).expect("serialize future plan");
    std::fs::write(&record.plan_path, &future).expect("replace plan");

    let error = load_plan_for_execution_in_store(&lifecycle_store, &record.run_id)
        .expect_err("future version rejected");
    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert!(error
        .message
        .contains("unsupported agent-task execution budget version 99"));
    assert_eq!(
        std::fs::read_to_string(&record.plan_path).expect("future plan retained"),
        future
    );
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

            let loaded = reconcile_status(run_id).expect("status resolves");
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
            record_lab_offload_submission_intent_in_store(
                &test_lifecycle_store(),
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
                assert!(!reconcile_pending_runner_submission_intent_in_store(
                    &test_lifecycle_store(),
                    run_id
                )
                .expect("fault is retained"));
            }
            assert!(reconcile_pending_runner_submission_intent_in_store(
                &test_lifecycle_store(),
                run_id
            )
            .expect("replay intent"));
            assert!(!reconcile_pending_runner_submission_intent_in_store(
                &test_lifecycle_store(),
                run_id
            )
            .expect("duplicate wake"));
            let record = reconcile_status(run_id).expect("accepted lifecycle");
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
fn pending_submission_owns_running_proxy_until_job_projection_arrives() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let store = JobStore::default();
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let _runner = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
            store: store.clone(),
            submitted: Arc::clone(&submitted),
            lookups: Arc::clone(&lookups),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let run_id = "delayed-runner-job-projection";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("record controller proxy");
        let request = replay_request(run_id, &command);
        record_lab_offload_submission_request(run_id, &request)
            .expect("persist pending broker request");
        let accepted_job = store
            .submit_remote_runner_job(request)
            .expect("broker accepts before response projection");
        rewrite_record_for_test(run_id, |record| {
            set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            // This is a runner-host PID. It cannot be probed by the controller.
            record.metadata["runner_pid"] = json!(u32::MAX);
        })
        .expect("simulate runner start before projection");

        assert_eq!(
            reconcile_stale_active_runs(false)
                .expect("remote PID does not make pending submission stale")
                .considered,
            0
        );

        let mut cancelled_job = accepted_job.clone();
        cancelled_job.status = homeboy_core::api_jobs::JobStatus::Cancelled;
        let expected_job_id = accepted_job.id.to_string();
        let _cancel = crate::agent_task_lifecycle::cancellation::test_cancel_hook::install(
            Box::new(move |runner_id, runner_job_id, durable_run_id| {
                assert_eq!(runner_id, "homeboy-lab");
                assert_eq!(runner_job_id, expected_job_id);
                assert_eq!(durable_run_id, run_id);
                Ok((cancelled_job.clone(), Vec::new()))
            }),
        );
        let cancelled = cancel_run(run_id, Some("operator cancellation"))
            .expect("cancellation binds and cancels accepted broker job");
        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            cancelled.runner_job_id(),
            Some(accepted_job.id.to_string().as_str())
        );
        assert_eq!(
            cancelled.metadata["live_cancellation"]["cancellation"],
            "runner_job_cancel"
        );
        assert_eq!(lookups.lock().expect("submission lookup").len(), 1);
        assert!(submitted.lock().expect("replay submissions").is_empty());
    });
}

#[test]
fn expired_running_submission_retains_typed_handoff_rejection() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let _runner = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
            store: JobStore::default(),
            submitted: Arc::new(Mutex::new(Vec::new())),
            lookups: Arc::new(Mutex::new(Vec::new())),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let run_id = "expired-running-submission";
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("record controller proxy");
        record_lab_offload_submission_request(run_id, &replay_request(run_id, &command))
            .expect("persist pending broker request");
        rewrite_record_for_test(run_id, |record| {
            set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record
                .lab_handoff
                .as_mut()
                .expect("pending handoff")
                .acceptance_deadline_at =
                Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
        })
        .expect("simulate rejected runner submission");

        let expired = reconcile_status(run_id).expect("expired handoff is terminalized");
        assert_eq!(expired.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            expired.metadata["cancel_reason"],
            EXPIRED_LAB_HANDOFF_REASON
        );
        assert_eq!(expired.metadata["phase"], "handoff_rejected");
    });
}

#[test]
fn cancelled_or_expired_pending_handoff_never_submits_new_runner_work() {
    with_isolated_home(|_| {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let _provider = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
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
            record_lab_offload_submission_intent_in_store(
                &test_lifecycle_store(),
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

        assert!(!reconcile_pending_runner_submission_intent_in_store(
            &test_lifecycle_store(),
            "cancel-before-admission"
        )
        .expect("cancelled handoff is not submitted"));
        assert!(!reconcile_pending_runner_submission_intent_in_store(
            &test_lifecycle_store(),
            "expire-before-admission"
        )
        .expect("expired handoff is not submitted"));
        assert!(submitted.lock().expect("submission log").is_empty());
        assert!(lookups.lock().expect("lookup log").is_empty());
    });
}

#[test]
fn preparing_crash_never_submits_or_queries_the_runner() {
    with_isolated_home(|_| {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let _provider = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
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
        record_lab_offload_submission_intent_in_store(
            &test_lifecycle_store(),
            "preparing-crash",
            "homeboy-lab",
            "/runner/workspace/homeboy",
            &command,
            &[],
        )
        .expect("preparing intent");

        assert!(!reconcile_pending_runner_submission_intent_in_store(
            &test_lifecycle_store(),
            "preparing-crash"
        )
        .expect("no replay"));
        assert_eq!(
            reconcile_status("preparing-crash").expect("status").state,
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
        let _provider = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
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
            record_lab_offload_submission_intent_in_store(
                &test_lifecycle_store(),
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
                let record = reconcile_status(run_id).expect("late acceptance reconciliation");
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
                assert_eq!(
                    crate::agent_task_lifecycle::workspace_authority::resolve_workspace_terminal_authority(
                        run_id,
                        "homeboy-lab",
                        "/runner/workspace/homeboy",
                        Some(&job_id),
                    )
                    .expect("terminal authority resolves")
                    .expect("terminal authority persisted")
                    .runner_job_id,
                    job_id,
                );
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
        record_lab_offload_submission_intent_in_store(
            &test_lifecycle_store(),
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
        let absent = reconcile_status("absent-after-deadline").expect("absent reconciliation");
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

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The retry runs through `retry_with_runtime_admission_in_store` with
/// `force = false` and `enforce_lineage_reservation = false` — the exact
/// arguments `retry_in_store` passes — and a stub admission, because controller
/// admission is machine-global by design and would otherwise reach the real
/// operator runtime store once the home is no longer mutated. That is the same
/// shape `submit_and_persist.rs:1490` already uses.
#[test]
fn retry_uses_controller_plan_when_runner_projection_replaces_plan_path() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "runner-projected-retry", |_| Ok(json!({})))
        .expect("controller plan submitted");
    let mut projected = lifecycle_store
        .read_record(&record.run_id)
        .expect("source record");
    projected.plan_path =
        "/home/chubes/.local/share/homeboy/agent-task-runs/runner-projected-retry/plan.json"
            .to_string();
    lifecycle_store
        .write_record(&projected)
        .expect("runner projection mirrored");

    let retry_record = retry_with_runtime_admission_in_store(
        &lifecycle_store,
        &record.run_id,
        Some("runner-projected-retry-local"),
        false,
        false,
        None,
        |_| Ok(json!({})),
    )
    .expect("local retry uses controller plan");
    assert_eq!(
        load_plan_in_store(&lifecycle_store, &retry_record.run_id).expect("retry plan"),
        plan
    );

    std::fs::remove_file(record.plan_path).expect("remove authoritative controller plan");
    let error = retry_with_runtime_admission_in_store(
        &lifecycle_store,
        &record.run_id,
        Some("missing-controller-plan"),
        false,
        false,
        None,
        |_| Ok(json!({})),
    )
    .expect_err("missing controller plan fails closed");
    assert_eq!(error.code, ErrorCode::InternalIoError);
}

/// Persisting a terminal Lab-bound record from inside a config-lock section is
/// the deadlock class behind #10751.
///
/// `store::write_record` reaches `workspace_authority::persist_terminal_from_record`,
/// which acquires the config lock, but only when the record is terminal *and*
/// Lab-runner bound *and* carries a `remote_workspace`. `flock(2)` is owned by
/// the open file description, so before the reentrancy repair that nested
/// acquisition blocked in the kernel forever, poisoning the process-wide
/// isolated-home mutex and wedging every remaining test in this binary.
///
/// #10754 removed the one nesting site it found (`store::mutate_record`); the
/// stall reappeared at `agent_task_service::execution`, which holds the lock
/// across `record_cook_attempt` -> `store::write_record`. This pins the shape
/// rather than either individual call site.
///
/// The write runs on a worker thread under a bounded wait so a regression fails
/// this test by name instead of consuming the entire CI test phase silently.
#[test]
fn terminal_lab_record_persists_from_inside_a_config_lock_section() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "config-lock-nested-terminal",
            runner_id: "homeboy-lab",
            runner_job_id: "job-nested",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("detached lab run");
        record.state = AgentTaskRunState::Succeeded;

        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(homeboy_core::config::with_config_lock(|| {
                store::write_record(&record)
            }));
        });

        receiver
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect(
                "persisting a terminal Lab-bound record inside a config-lock section deadlocked; \
                 the config lock is not reentrancy-safe",
            )
            .expect("terminal record persists");

        let stored = store::read_record("config-lock-nested-terminal").expect("stored record");
        assert_eq!(stored.state, AgentTaskRunState::Succeeded);

        let authority =
            crate::agent_task_lifecycle::workspace_authority::resolve_workspace_terminal_authority(
                "config-lock-nested-terminal",
                "homeboy-lab",
                "/runner/workspace/homeboy",
                Some("job-nested"),
            )
            .expect("terminal authority resolves");
        assert!(
            authority.is_some(),
            "the nested write must still persist terminal workspace authority, not skip it"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The mirrored transport event is written onto this record and the
/// log projection is read back off it, so the events the log exposes are the
/// event this test persisted. The canonical event keeps the bounded transport
/// payload under `data.transport` rather than exposing a second raw stream.
#[test]
fn logs_expose_mirrored_live_runner_events_before_terminal_aggregate() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "live-runner-events",
            runner_id: "homeboy-lab",
            runner_job_id: "job-live",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
    )
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
    lifecycle_store
        .write_record(&record)
        .expect("persist mirrored event");

    let log = logs_in_store(&lifecycle_store, "live-runner-events").expect("live logs resolve");

    assert_eq!(log.events.len(), 1);
    assert!(log.events[0].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider started")));
    assert_eq!(
        log.events[0].data["provider"].as_str(),
        Some("openai/gpt-5.6-terra")
    );
    assert_eq!(log.events[0].data["phase"], "implementing");
    assert_eq!(
        log.events[0].data["activity"].as_str(),
        Some("editing lifecycle projection")
    );
    assert_eq!(log.events[0].data["heartbeat_at_ms"], 42);
    assert_eq!(
        log.events[0].data["transport"]["provider"],
        "openai/gpt-5.6-terra"
    );
    assert_eq!(
        log.events[0].data["transport"]["activity"],
        "editing lifecycle projection"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Acceptance validates the runner identity a *previously persisted*
/// record already holds, so the rejection only proves anything if the second
/// acceptance is validated against the record the first one wrote. Reading a
/// different home there would either reject for the wrong reason or find no
/// record to conflict with at all.
///
/// The runner-continuation registry `reconcile_status_in_store` consults stays
/// process-global by design: it is configured trust material and a subprocess
/// contract, not a lifecycle root (#12618).
#[test]
fn terminal_lab_artifact_attachment_refuses_runner_provenance_mismatch() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-late-artifact-mismatch",
            runner_id: "homeboy-lab",
            runner_job_id: "job-original",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact-mismatch",
            remote_command: &command,
        },
    )
    .expect("running proxy");
    let mut record = reconcile_status_in_store(
        &lifecycle_store,
        "agent-task-late-artifact-mismatch",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status")
    .record;
    apply_runner_job_terminal_state(
        &mut record,
        homeboy_core::api_jobs::JobStatus::Succeeded,
        &[],
    );
    lifecycle_store
        .write_record(&record)
        .expect("terminal record");

    let error = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-late-artifact-mismatch",
            runner_id: "different-lab",
            runner_job_id: "job-artifact-attach",
            remote_workspace: "/home/lab/agent-task-runs/agent-task-late-artifact-mismatch",
            remote_command: &command,
        },
    )
    .expect_err("artifact provenance must retain its original runner");
    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(error.details["field"], "lab_handoff");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Both handoffs and both reconciliations are made through siblings
/// handed `lifecycle_store`; the terminal snapshot is supplied as a parameter,
/// so no runner subsystem is consulted and the process-global
/// runner-continuation registry stays untouched.
#[test]
fn accepted_handoff_waits_for_authoritative_aggregate_after_terminal_daemon_status() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
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
        let mut record = record_detached_lab_run_in_store(
            &lifecycle_store,
            DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            },
        )
        .expect("accepted handoff");
        let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        snapshot.job.status = job_status;
        snapshot.events.clear();

        reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
            .expect("terminal daemon result records pending synchronization");

        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(record.metadata["runner_job_status"], json!(job_status));
        assert_eq!(
            record.metadata["runner_result_synchronization"]["state"],
            "pending"
        );
        assert_eq!(record.metadata["phase"], "awaiting_runner_synchronization");
    }
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
        let persisted = reconcile_status(&record.run_id).expect("persisted controller record");
        assert_eq!(persisted.state, AgentTaskRunState::Running);
        assert!(persisted.artifact_refs.is_empty());
        assert!(store::read_aggregate(&record.run_id).is_err());
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The fixture deliberately points `plan_path` at a *runner-local*
/// path that does not exist here, and the claim is that hydration succeeds
/// without reading it. Both writes, both reconciliations, the status, the
/// artifact reports and the aggregate read all have to name one installation —
/// and the closing idempotence assertion (still exactly one artifact after a
/// replay) is a population count over that one home.
#[test]
fn terminal_proxy_reconciliation_hydrates_persisted_nested_result_idempotently() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-persisted-result",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
    )
    .expect("running proxy");
    record.plan_path =
        "/home/lab/.local/share/homeboy/agent-task-runs/agent-task-persisted-result/plan.json"
            .to_string();
    lifecycle_store
        .write_record(&record)
        .expect("runner-local plan projection");
    apply_runner_job_terminal_state(
        &mut record,
        homeboy_core::api_jobs::JobStatus::Succeeded,
        &[],
    );
    lifecycle_store
        .write_record(&record)
        .expect("legacy terminal projection without aggregate");

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

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("hydrate persisted result");
    let status = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("hydrated status without runner plan access")
    .record;
    let artifact_report =
        artifacts_in_store(&lifecycle_store, &record.run_id).expect("hydrated artifacts");
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
    let aggregate = lifecycle_store
        .read_aggregate(&record.run_id)
        .expect("persisted authoritative aggregate");
    let review = crate::agent_task_aggregate::AgentTaskAggregateReport::from(aggregate.outcomes);
    assert_eq!(review.summary.apply_candidates, 1);
    assert_eq!(review.apply_candidates[0].artifact_ids, vec!["final-patch"]);

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("idempotent replay");
    assert_eq!(
        artifacts_in_store(&lifecycle_store, &record.run_id)
            .expect("replayed artifacts")
            .artifacts
            .len(),
        1
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Reconciliation is not a pure function of the in-memory record it
/// mutates: it binds the pending handoff, reads the aggregate to decide
/// idempotence and commits the terminal projection, all durably. Those writes
/// and that read have to name the installation the planned proxy was persisted
/// into, or the terminal record asserted below would be reconciled from one
/// home's aggregate and committed into another's (#7505).
///
/// The proxy is created through the `*_with_submission_in_store` form because
/// the default Lab-offload submission admits through the machine-global
/// controller-runtime store; see `stub_lab_offload_submission`. Nothing here
/// asserts on controller-runtime provenance, so the stub pin is invisible to
/// this test.
#[test]
fn transport_proxy_snapshot_reconciliation_advances_queued_lifecycle() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
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
    reconcile_transport_proxy_snapshot_in_store(
        &lifecycle_store,
        &mut record,
        &terminal_child_snapshot(&aggregate),
    )
    .expect("transport proxy reconciliation");

    assert_eq!(record.state, AgentTaskRunState::Succeeded);
    assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
    assert_eq!(record.metadata["runner_job_status"], "succeeded");
    assert_eq!(
        record.metadata["runner_execution_record"]["status"],
        "succeeded"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Artifact projection writes bytes into an artifact root and an index
/// row into an observation database, and this test then resolves the logical id
/// back out of both. `ObservationStore::open_initialized_in_roots` is the rooted
/// counterpart of the ambient `open_initialized()` — same maintenance mode, both
/// the database and the artifact root bound from the same `PathRoots` the
/// lifecycle store projected into. Opening the ambient store here would index an
/// injected artifact root against another home's database, which is the exact
/// split #7505 exists to stop.
#[test]
fn terminal_executor_artifacts_are_projected_under_logical_ids() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
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
    for (id, kind, bytes) in [
        ("transcript", "transcript", b"transcript bytes".as_slice()),
        (
            "agent-result",
            "agent-result",
            b"agent result bytes".as_slice(),
        ),
    ] {
        let artifact = root.path().join(id);
        std::fs::write(&artifact, bytes).expect("write terminal artifact");
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some(artifact.display().to_string()),
            url: None,
            mime: Some("text/plain".to_string()),
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(homeboy_core::artifact_metadata::sha256_file(&artifact).expect("sha")),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
    }
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "projection-parity", |_| Ok(json!({})))
        .expect("submit");
    record_run_aggregate_in_store(&lifecycle_store, "projection-parity", &plan, &aggregate)
        .expect("record aggregate");
    reconcile_terminal_artifact_projection_in_store(&lifecycle_store, "projection-parity")
        .expect("idempotent projection");
    let record = reconcile_status_in_store(
        &lifecycle_store,
        "projection-parity",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("terminal record")
    .record;
    assert_eq!(
        record.metadata["artifact_projection"]["status"], "complete",
        "{:#}",
        record.metadata
    );

    let store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )
    .expect("store");
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
    for (id, bytes) in [
        ("transcript", b"transcript bytes".as_slice()),
        ("agent-result", b"agent result bytes".as_slice()),
    ] {
        let artifact = homeboy_core::observation::runs_service::resolve_artifact_for_run(
            &store,
            "projection-parity",
            id,
        )
        .expect("resolve logical terminal artifact");
        assert_eq!(
            std::fs::read(artifact.path).expect("projected bytes"),
            bytes
        );
    }
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The closing assertion is that the tampered artifact was *not*
/// indexed, so the observation store queried has to be the one the rejected
/// projection would have written into — the rooted opener binds both it and the
/// artifact root from the same `PathRoots` the lifecycle store used.
#[test]
fn terminal_executor_artifact_projection_rejects_mismatched_bytes() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let root = tempfile::tempdir().expect("executor artifact root");
    let patch = root.path().join("patch.diff");
    std::fs::write(&patch, "expected patch").expect("write patch");
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
        size_bytes: Some("expected patch".len() as u64),
        sha256: Some(homeboy_core::artifact_metadata::sha256_file(&patch).expect("sha")),
        metadata: json!({ "executor_artifact_finalized": true }),
    });
    std::fs::write(&patch, "tampered patch").expect("tamper patch");
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "projection-tampered", |_| Ok(json!({})))
        .expect("submit");
    record_run_aggregate_in_store(&lifecycle_store, "projection-tampered", &plan, &aggregate)
        .expect("record aggregate");

    let record = reconcile_status_in_store(
        &lifecycle_store,
        "projection-tampered",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("terminal record")
    .record;
    assert_eq!(record.metadata["artifact_projection"]["status"], "failed");
    assert!(record.metadata["artifact_projection"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("does not match")));
    let store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )
    .expect("store");
    assert!(
        homeboy_core::observation::runs_service::resolve_artifact_for_run(
            &store,
            "projection-tampered",
            "patch",
        )
        .is_err()
    );
}

#[test]
fn controller_leaves_runner_artifact_projection_pending_when_it_cannot_mirror_bytes() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts = vec![
            AgentTaskArtifact {
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "report".to_string(),
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

        let record = reconcile_status(&submitted.run_id).expect("status");
        assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
        assert!(record.metadata["artifact_projection"]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
        assert_eq!(
            record.metadata["artifact_projection"]["recovery_action"]["command"],
            format!("homeboy agent-task status {}", submitted.run_id)
        );
        assert!(
            run_owes_candidate_follow_up_in_store(&test_lifecycle_store(), &submitted.run_id)
                .expect("pending import retains the runner workspace")
        );
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
fn duplicate_runner_artifact_ids_fail_closed_before_projection() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        let bytes = b"runner patch";
        let artifact = AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/runner/private/patch.diff".to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            metadata: json!({ "executor_artifact_finalized": true }),
        };
        aggregate.outcomes[0].artifacts.push(artifact.clone());
        let mut duplicate_outcome = aggregate.outcomes[0].clone();
        duplicate_outcome.task_id = "task-b".to_string();
        duplicate_outcome.artifacts = vec![artifact];
        aggregate.outcomes.push(duplicate_outcome);

        let submitted = submit_plan(&plan, Some("duplicate-runner-artifact")).expect("submit");
        record_runner_job_identity(&submitted.run_id, "homeboy-lab", "job-1")
            .expect("runner identity");
        record_run_aggregate(&submitted.run_id, &plan, &aggregate).expect("record aggregate");

        let record = reconcile_status(&submitted.run_id).expect("terminal record");
        assert_eq!(record.metadata["artifact_projection"]["status"], "failed");
        assert!(record.metadata["artifact_projection"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("reuses artifact id 'patch'")));
        assert!(
            run_owes_candidate_follow_up_in_store(&test_lifecycle_store(), &submitted.run_id)
                .expect("duplicate identity retains the runner workspace")
        );
        let artifacts = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .list_artifacts(&submitted.run_id)
            .expect("artifact records");
        assert!(
            artifacts.is_empty(),
            "no ambiguous artifact may be imported"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Both promotion writes and the record they are read back from are
/// the injected store's, so `latest_promotion` and the promotion history length
/// asserted below describe one home rather than a mixture of two.
#[test]
fn corrected_promotion_replaces_gate_failed_latest_proof() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let run_id = "run-corrected-promotion";
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");

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

    record_promotion_in_store(&lifecycle_store, run_id, gate_failed)
        .expect("gate failure recorded");
    let gate_failed_record = lifecycle_store
        .read_record(run_id)
        .expect("gate failure lifecycle state");
    assert_eq!(
        gate_failed_record.state,
        AgentTaskRunState::CandidateRecoverable,
        "a promoted candidate blocked by gates remains recoverable rather than successful"
    );
    let updated = record_promotion_in_store(&lifecycle_store, run_id, corrected.clone())
        .expect("correction recorded");

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
    assert_eq!(updated.state, AgentTaskRunState::CandidateRecoverable);
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
                task_id: "cook-conductor".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                outputs: serde_json::json!({}),
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "provider_run_result": {
                        "schema": "custom-provider/agent-task-run-result/v1",
                        "status": "failed",
                        "failure_classification": "runtime"
                    }
                }),
                ..Default::default()
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

        let loaded = reconcile_status("local-sparse-run").expect("status loaded");
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

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Status may persist current admission and reconciliation projections;
/// the invariant here is that existing terminal provider evidence is unchanged.
///
/// `record_completed_run_in_store` reaches automatic artifact retention only
/// when a task declares a workspace root that exists; `test_plan()` declares
/// none, so no ambient retention pass is entered here.
#[test]
fn status_preserves_existing_terminal_runtime_evidence() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let mut plan = test_plan();
    plan.tasks[0].executor.backend = "opencode".to_string();
    let aggregate = succeeded_aggregate(&plan);
    let record = record_completed_run_in_store(
        &lifecycle_store,
        &plan,
        &aggregate,
        Some("existing-runtime"),
    )
    .expect("terminal record");
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record.lifecycle.provider_runtime[0].metadata = json!({
            "evidence_source": "native_provider",
            "manual": true,
        });
    })
    .expect("native evidence persisted");
    let before = lifecycle_store
        .read_record(&record.run_id)
        .expect("record before status");

    let loaded = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status preserves runtime evidence")
    .record;
    let after = lifecycle_store
        .read_record(&record.run_id)
        .expect("record after status");

    assert_eq!(
        loaded.lifecycle.provider_runtime,
        before.lifecycle.provider_runtime
    );
    assert_eq!(
        after.lifecycle.provider_runtime,
        before.lifecycle.provider_runtime
    );
    assert_eq!(
        loaded.lifecycle.provider_runtime[0].metadata["manual"],
        true
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). `records.len() == 1` is a population count over one installation's
/// run directory — read ambiently it would count the operator's real runs — and
/// the four projections (status, logs, artifacts, existence) are all asserted
/// to describe the single run this test recorded under an unsanitized id.
///
/// `record_completed_run_in_store` reaches automatic artifact retention only
/// when a task declares a workspace root that exists; this plan declares only a
/// cleanup mode, so no ambient retention pass is entered here.
#[test]
fn lifecycle_store_round_trips_record_log_artifacts_and_lifecycle_contract() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
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

    let record = record_completed_run_in_store(
        &lifecycle_store,
        &plan,
        &aggregate,
        Some("run/store-contract"),
    )
    .expect("completed run recorded");
    let loaded = reconcile_status_in_store(
        &lifecycle_store,
        "run/store-contract",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status loaded by unsanitized id")
    .record;
    let log = logs_in_store(&lifecycle_store, "run/store-contract")
        .expect("logs loaded by unsanitized id");
    let artifact_report = artifacts_in_store(&lifecycle_store, "run/store-contract")
        .expect("artifacts loaded by unsanitized id");
    let records = list_records_in_store(&lifecycle_store).expect("records listed");

    assert_eq!(record.run_id, "run_store-contract");
    assert!(
        run_record_exists_in_store(&lifecycle_store, "run/store-contract").expect("record exists")
    );
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
    assert_eq!(
        log.schema,
        homeboy_control_plane_contract::CONTROL_PLANE_EVENT_PAGE_SCHEMA
    );
    assert_eq!(log.events[0].data["state"], "succeeded");
    assert_eq!(artifact_report.schema, schemas::RUN_ARTIFACTS);
    assert_eq!(artifact_report.artifacts[0].id, "patch");
    assert_eq!(artifact_report.evidence_refs[0].kind, "transcript");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, "run_store-contract");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The successor identity, the inherited notification route, and the
/// plan read back for it all follow the injected root, so the retry lineage
/// asserted below is the one this store recorded. The admission is stubbed for
/// the same reason as `retry_uses_controller_plan_when_runner_projection_replaces_plan_path`.
#[test]
fn retry_submits_new_run_from_existing_plan() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-original", |_| Ok(json!({})))
        .expect("submitted");
    let mut source = lifecycle_store.read_record("run-original").expect("source");
    source.metadata["notification_route"] = json!({
        "transport": "extension",
        "route": "opaque-origin"
    });
    lifecycle_store
        .write_record(&source)
        .expect("route persisted");

    let record = retry_with_runtime_admission_in_store(
        &lifecycle_store,
        "run-original",
        Some("run-retry"),
        false,
        false,
        None,
        |_| Ok(json!({})),
    )
    .expect("retry submitted");
    let loaded_plan = load_plan_in_store(&lifecycle_store, "run-retry").expect("retry plan loaded");

    assert_eq!(record.run_id, "run-retry");
    assert_eq!(record.state, AgentTaskRunState::Queued);
    assert_eq!(record.metadata["retry_of"], json!("run-original"));
    assert_eq!(
        record.metadata["notification_route"]["route"],
        "opaque-origin"
    );
    assert_eq!(loaded_plan.plan_id, "plan-a");
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The stale record is seeded and reclaimed through the same injected
/// store, so the reclaim evidence asserted below cannot have been produced by
/// another home holding this run id.
#[test]
fn mark_running_reclaims_stale_running_record() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-stale-dead-owner", |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store
        .read_record("run-stale-dead-owner")
        .expect("record");
    record.state = AgentTaskRunState::Running;
    record.metadata = json!({ "runner_pid": u32::MAX });
    lifecycle_store
        .write_record(&record)
        .expect("stored stale record");

    let running =
        mark_running_in_store(&lifecycle_store, "run-stale-dead-owner").expect("reclaimed");

    assert_eq!(running.state, AgentTaskRunState::Running);
    assert_eq!(running.metadata["reclaimed_stale_running"], json!(true));
    assert_eq!(running.metadata["runner_pid"], json!(std::process::id()));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The owned process identity is planted through the same store the
/// cancellation reads, so the pids it signals are the ones this test spawned.
/// The run carries no runner id or runner job id, so `cancel_run_in_store`
/// never enters `classify_live_cancellation`'s runner-backed branch and the
/// process-global runner-continuation registry is not consulted.
#[cfg(unix)]
#[test]
fn cancel_run_signals_live_running_record() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let identity = homeboy_core::build_identity::current().display;
    let artifact = context.root().join("fake-controller");
    let digest = fake_controller_artifact(&artifact, &identity, "live cancellation fixture");
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-cancel-live", |_| {
            Ok(json!({
                "originating": {
                    "build_identity": identity,
                    "pinned_executable": artifact,
                    "sha256": digest,
                }
            }))
        })
        .expect("submitted");
    // Drop the stub admission before marking running (#12721).
    //
    // `mark_running_in_store` migrates the pin before it validates it, and
    // migration skips only when the key is ABSENT -- an empty object is
    // present, so `migrate_legacy_pin_unlocked` demands
    // `/originating/build_identity` and fails closed. That contract is
    // correct: `{}` is malformed durable metadata and refusing to mutate on
    // it is the point. What is wrong is persisting `{}` in the first place.
    //
    // Cancellation reads `runner_pid` and never touches the runtime pin, so
    // the record this test needs is one with no pin at all. This mirrors
    // `mark_running_reclaims_stale_running_record`, which uses the same stub
    // and passes only because it replaces metadata wholesale first.
    let mut submitted = lifecycle_store
        .read_record("run-cancel-live")
        .expect("submitted record");
    submitted
        .metadata
        .as_object_mut()
        .expect("record metadata is an object")
        .remove(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY);
    lifecycle_store
        .write_record(&submitted)
        .expect("drop stub controller runtime pin");

    mark_running_in_store(&lifecycle_store, "run-cancel-live").expect("marked running");

    // The test binary cannot be a cancellation target: process cleanup
    // correctly excludes the current PID. Use a separate owner with a
    // descendant that ignores SIGTERM, proving the SIGKILL path reaps both.
    let mut child = std::process::Command::new("sh")
        .args([
            "-c",
            "trap '' TERM; (trap '' TERM; exec sleep 30) & echo $!; wait",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn live owned process tree");
    let stdout = child.stdout.take().expect("child stdout");
    let mut stdout = BufReader::new(stdout);
    let mut descendant_pid = String::new();
    stdout
        .read_line(&mut descendant_pid)
        .expect("read descendant pid");
    let descendant_pid: u32 = descendant_pid.trim().parse().expect("descendant pid");
    let owner_pid = child.id();
    let owner_identity = homeboy_core::process::process_start_identity(owner_pid)
        .expect("probe owner process identity")
        .expect("live owner exposes a process identity");
    let mut running = lifecycle_store
        .read_record("run-cancel-live")
        .expect("running record");
    running.metadata["runner_pid"] = json!(owner_pid);
    running.metadata["runner_process_start_identity"] = json!(owner_identity);
    lifecycle_store
        .write_record(&running)
        .expect("persist owned process identity");

    let cancelled =
        cancel_run_in_store(&lifecycle_store, "run-cancel-live", None).expect("live run cancelled");

    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
    assert_eq!(
        cancelled.metadata["live_cancellation"]["owner_pid"],
        json!(owner_pid)
    );
    assert_eq!(
        cancelled.metadata["live_cancellation"]["signal"],
        json!("SIGKILL")
    );
    assert!(cancelled.metadata["live_cancellation"]["killed_pids"]
        .as_array()
        .expect("SIGKILL targets")
        .iter()
        .any(|pid| pid == &json!(descendant_pid)));
    assert!(!homeboy_core::process::pid_is_running(owner_pid));
    assert!(!homeboy_core::process::pid_is_running(descendant_pid));
}

#[test]
fn record_health_migrates_legacy_and_quarantines_conflicting_projections() {
    with_isolated_home(|_| {
        submit_plan(&test_plan(), Some("legacy-record")).expect("submitted");
        // #11446 made the typed writer reject unsupported schemas, which is the
        // point of the guard but also blocks planting the very legacy record
        // this test migrates. Use the raw injector built for that.
        crate::agent_task_lifecycle::inject_raw_record_metadata_for_corruption_test(
            "legacy-record",
            |value| {
                value["agent_task_run"]["schema"] = json!("homeboy/agent-task-run/v0");
            },
        )
        .expect("legacy stored");
        let legacy = reconcile_record_health_in_store(&test_lifecycle_store(), false)
            .expect("legacy migrated");
        assert_eq!(legacy.migrated, 1);
        let legacy_record = reconcile_status("legacy-record").expect("legacy loaded");
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
        let dry_run = reconcile_record_health_in_store(&test_lifecycle_store(), true)
            .expect("conflict dry run");
        assert_eq!(
            dry_run.records[0].reason,
            AgentTaskRecordHealthReason::ConflictingProjections
        );
        assert_eq!(dry_run.records[0].action, "would-quarantine");
        let applied = reconcile_record_health_in_store(&test_lifecycle_store(), false)
            .expect("conflict quarantined");
        assert_eq!(applied.quarantined, 1);
        let health =
            record_health_summary_in_store(&test_lifecycle_store()).expect("quarantine health");
        assert_eq!(health.conflicting, 1);
        assert_eq!(health.quarantined, 1);
        assert_eq!(
            reconcile_record_health_in_store(&test_lifecycle_store(), false)
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
        let malformed_handoff = json!({
            "state": "pending",
            "authority": "controller",
            "runner_id": "homeboy-lab",
            "submitted_at": "invalid"
        });
        let validation_error = rewrite_record_for_test("malformed-typed-pending", |record| {
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
                workspace_identity: None,
                workspace_lifecycle_revision: 0,
                workspace_owner_lease: None,
                workspace_claim: None,
            });
        })
        .expect_err("validated test rewrite rejects malformed typed handoff");
        assert_eq!(validation_error.code, ErrorCode::InternalJsonError);

        inject_raw_record_metadata_for_corruption_test("malformed-typed-pending", |metadata| {
            metadata["agent_task_run"]["lab_handoff"] = malformed_handoff;
        })
        .expect("raw corruption fixture stored");

        let health =
            record_health_summary_in_store(&test_lifecycle_store()).expect("health report");
        assert_eq!(health.malformed, 1);
        let report = reconcile_record_health_in_store(&test_lifecycle_store(), false)
            .expect("quarantine malformed state");
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

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The log projection reads back the aggregate the completed run
/// wrote, so `log.events.len() == 2` and the per-event envelope fields
/// describe the aggregate this test recorded rather than whatever an ambient
/// home holds under this run id.
///
/// `record_completed_run_in_store` reaches automatic artifact retention only
/// when a task declares a workspace root that exists; `test_plan()` declares
/// none, so no ambient retention pass is entered here.
#[test]
fn logs_return_the_canonical_control_plane_event_page() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
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

    record_completed_run_in_store(
        &lifecycle_store,
        &plan,
        &aggregate,
        Some("run-event-envelope"),
    )
    .expect("recorded");

    let log = logs_in_store(&lifecycle_store, "run-event-envelope").expect("logs");

    assert_eq!(
        log.schema,
        homeboy_control_plane_contract::CONTROL_PLANE_EVENT_PAGE_SCHEMA
    );
    assert_eq!(log.events.len(), 2);
    assert_eq!(
        log.events[0].schema,
        homeboy_control_plane_contract::CONTROL_PLANE_EVENT_SCHEMA
    );
    assert_eq!(log.events[0].run.as_str(), "run-event-envelope");
    assert_eq!(
        log.events[0].task.as_ref().map(|id| id.as_str()),
        Some("task-a")
    );
    assert_eq!(log.events[0].sequence, 1);
    assert_eq!(log.events[0].data["state"], "running");
    assert_eq!(log.events[1].data["message"], "ok");
    assert_eq!(log.events[1].artifacts.len(), 1);
}

#[test]
fn set_run_state_stamps_finished_at_for_candidate_recoverable_terminal_runs() {
    // A run that finished with a recoverable candidate is terminal, so
    // set_run_state must stamp finished_at for it exactly as it does for the
    // other terminal states. Regression guard for the drift where the setter's
    // hand-listed terminal subset omitted CandidateRecoverable, leaving these
    // runs without a finished_at while the legacy-record migration path stamped
    // one.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). `set_run_state` is a pure in-memory setter; the store is only
    // needed to own the submitted record the two rewrites read and write back.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(
            &test_plan(),
            "candidate-recoverable-finished-at",
            |_| Ok(json!({})),
        )
        .expect("submit");
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        set_run_state(record, AgentTaskRunState::CandidateRecoverable);
        assert_eq!(record.state, AgentTaskRunState::CandidateRecoverable);
        assert!(
            record.lifecycle.execution.finished_at.is_some(),
            "a terminal CandidateRecoverable run must stamp finished_at"
        );
    })
    .expect("rewrite record");

    // And a non-terminal state must NOT stamp finished_at.
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |record| {
        record.lifecycle.execution.finished_at = None;
        set_run_state(record, AgentTaskRunState::Running);
        assert!(
            record.lifecycle.execution.finished_at.is_none(),
            "a non-terminal Running run must not stamp finished_at"
        );
    })
    .expect("rewrite record running");
}

/// A runner-continuation provider that counts every runner interaction so a
/// test can assert that a read performed *none* (#10418).
struct CountingRunnerProvider {
    interactions: Arc<std::sync::atomic::AtomicUsize>,
}

impl CountingRunnerProvider {
    fn record(&self) {
        self.interactions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl RunnerContinuationProvider for CountingRunnerProvider {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        self.record();
        Err(Error::internal_unexpected("counted runner snapshot"))
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        self.record();
        true
    }

    fn runner_authority(&self, _runner_id: &str) -> RunnerAuthority {
        self.record();
        RunnerAuthority::Configured
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        self.record();
        Err(Error::internal_unexpected("counted runner exec"))
    }

    fn submit_reverse_broker_job(
        &self,
        _runner_id: &str,
        _request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        self.record();
        Err(Error::internal_unexpected("counted reverse broker job"))
    }
}

#[test]
fn controller_local_status_answers_without_any_runner_interaction() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let interactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _provider = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider {
            interactions: interactions.clone(),
        }));

        let record = submit_plan(&test_plan(), Some("controller-local-status")).expect("submit");
        mark_running(&record.run_id).expect("mark running");

        let outcome =
            reconcile_status_with_options(&record.run_id, AgentTaskStatusOptions::default())
                .expect("controller-local status resolves");

        // The whole point of #10418: a known controller-local run must be
        // answerable while the Lab is wedged, so the read must not reach the
        // runner subsystem at all.
        assert_eq!(
            interactions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a controller-local status must not interact with any runner"
        );
        assert_eq!(outcome.record.state, AgentTaskRunState::Running);
        assert!(outcome.runner_probe.controller_local);
        assert!(!outcome.runner_probe.performed);
        assert_eq!(
            outcome.runner_probe.skipped_reason,
            Some(RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL)
        );
    });
}

#[test]
fn a_runner_backed_status_still_reconciles_against_the_runner() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let interactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _provider = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider {
            interactions: interactions.clone(),
        }));

        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "runner-backed-status",
            runner_id: "homeboy-lab",
            runner_job_id: "job-10418",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff recorded");

        let outcome = reconcile_status_with_options(
            "runner-backed-status",
            AgentTaskStatusOptions::default(),
        )
        .expect("runner-backed status resolves");

        assert!(
            interactions.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "a runner-backed running record must still reconcile against its runner"
        );
        assert!(!outcome.runner_probe.controller_local);
        assert!(outcome.runner_probe.performed);
        assert_eq!(outcome.runner_probe.skipped_reason, None);
    });
}

#[test]
fn a_caller_can_opt_a_runner_backed_status_out_of_every_runner_probe() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let interactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _provider = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider {
            interactions: interactions.clone(),
        }));

        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "runner-backed-local-only",
            runner_id: "homeboy-lab",
            runner_job_id: "job-10418-local",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff recorded");

        let outcome = reconcile_status_with_options(
            "runner-backed-local-only",
            AgentTaskStatusOptions {
                runner_probe: AgentTaskRunnerProbe::Never,
            },
        )
        .expect("local-only status resolves");

        assert_eq!(
            interactions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "--no-runner-probe must not reach the runner"
        );
        assert!(!outcome.runner_probe.performed);
        assert_eq!(
            outcome.runner_probe.skipped_reason,
            Some(RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT)
        );
    });
}

#[test]
fn durable_aggregate_read_returns_partial_local_evidence_without_a_runner_probe() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let interactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _provider = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider {
            interactions: interactions.clone(),
        }));

        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "runner-backed-durable-read",
            runner_id: "homeboy-lab",
            runner_job_id: "job-11166",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff recorded");

        let snapshot =
            durable_local_read("runner-backed-durable-read").expect("durable local read resolves");
        let artifacts =
            artifacts("runner-backed-durable-read").expect("durable local artifacts resolve");

        assert_eq!(snapshot.record.run_id, "runner-backed-durable-read");
        assert!(snapshot.aggregate.is_none());
        assert_eq!(snapshot.unavailable_sources.len(), 1);
        assert_eq!(snapshot.unavailable_sources[0].source, "aggregate");
        assert_eq!(
            snapshot.unavailable_sources[0].reason_code,
            "durable_read.authoritative_aggregate_absent"
        );
        assert_eq!(artifacts.run_id, "runner-backed-durable-read");
        assert_eq!(
            interactions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "aggregate readers must return local partial evidence without probing an unavailable runner"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The oversized fixture is written at the path the injected store
/// itself names and read back through the sibling handed the same store, so the
/// partial-read evidence asserted below is about this home's aggregate file.
///
#[test]
fn durable_aggregate_read_does_not_pair_a_record_with_an_unmirrored_cache() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "oversized-durable-aggregate", |_| {
            Ok(json!({}))
        })
        .expect("durable record");
    let path = lifecycle_store.aggregate_path(&record.run_id);
    std::fs::write(
        &path,
        vec![b'x'; (store::DURABLE_AGGREGATE_MAX_BYTES + 1) as usize],
    )
    .expect("oversized aggregate fixture");

    let snapshot = durable_local_read_in_store(&lifecycle_store, &record.run_id)
        .expect("partial durable read");

    assert_eq!(snapshot.record.run_id, record.run_id);
    assert!(snapshot.aggregate.is_none());
    assert_eq!(snapshot.unavailable_sources.len(), 1);
    assert_eq!(snapshot.unavailable_sources[0].source, "aggregate");
    assert_eq!(
        snapshot.unavailable_sources[0].reason_code,
        "durable_read.authoritative_aggregate_absent"
    );
}

#[test]
fn durable_aggregate_read_returns_within_the_readonly_sqlite_contention_budget() {
    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("contended-durable-read")).expect("record");
        let path = homeboy_core::observation::store::database_path().expect("database path");
        let lock = rusqlite::Connection::open(path).expect("lock connection");
        lock.execute_batch(
            "PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; UPDATE runs SET status = status;",
        )
        .expect("exclusive lock");

        let started = std::time::Instant::now();
        let error = durable_local_read(&record.run_id).expect_err("contended read fails bounded");

        assert_eq!(error.code, ErrorCode::ObservationStoreBusy);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
}

#[test]
fn runner_backed_logs_read_persisted_events_without_a_runner_probe() {
    super::ensure_runner_continuation_provider_reset_hook();
    with_isolated_home(|_| {
        let interactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _provider = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider {
            interactions: interactions.clone(),
        }));

        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "runner-backed-logs-local-only",
            runner_id: "homeboy-lab",
            runner_job_id: "job-10418-logs",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff recorded");

        let log = logs("runner-backed-logs-local-only").expect("durable logs resolve");

        assert_eq!(log.run.as_str(), "runner-backed-logs-local-only");
        assert_eq!(
            interactions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "logs must not wait for an unavailable runner"
        );
    });
}
