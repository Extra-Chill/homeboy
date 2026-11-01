//! The single `cook_delivery` writer (#16049), exercised through real store
//! writes.

use super::*;
use crate::agent_task_service::{
    recorded_finalization_receipt, recorded_promotion_report, CookDelivery,
    CookDeliveryBlockerCode, CookDeliveryState, COOK_DELIVERY_SCHEMA,
};
use homeboy_core::cook_status::CookStatus;

fn store_with_run(
    context: &homeboy_core::test_support::HermeticTestContext,
    run_id: &str,
) -> AgentTaskLifecycleStore {
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    lifecycle_store
}

fn promotion(run_id: &str, status: &str, gates: Value) -> Value {
    json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": status,
        "source": { "kind": "aggregate", "task_id": "task-a", "run_id": run_id },
        "to_worktree": "homeboy@cook",
        "target": { "worktree": "homeboy@cook", "path": "/work/homeboy", "branch": "cook/fix", "head": "abc123" },
        "candidate": { "base_sha": "base", "candidate_sha": "candidate.patch", "tree_sha": "patchsha" },
        "changed_files": ["src/lib.rs"],
        "verified_base": { "base": "origin/main", "sha": "base-sha" },
        "deterministic_gates": gates,
        "provenance": {
            "post_apply": true,
            "worktree_path": "/work/homeboy",
            "candidate": { "kind": "git", "fingerprint": { "sha256": "fingerprint-sha", "head": "abc123" } }
        }
    })
}

fn gate(status: &str) -> Value {
    json!([{ "id": "cargo-test", "status": status, "command": ["cargo", "test"] }])
}

fn published() -> Value {
    json!({
        "status": "review_ready",
        "path": "/work/homeboy",
        "base": "main",
        "head": "cook/fix",
        "pr_action": "created",
        "pr_number": 42,
        "pr_url": "https://github.com/Extra-Chill/homeboy/pull/42",
        "manual_finalization": false,
    })
}

fn persisted_delivery(record: &AgentTaskRunRecord) -> CookDelivery {
    assert_eq!(
        record.metadata["cook_delivery"]["schema"], COOK_DELIVERY_SCHEMA,
        "every delivery write persists the versioned record"
    );
    CookDelivery::from_run_metadata(&record.metadata).expect("delivery")
}

#[test]
fn the_record_is_the_only_stored_form_of_delivery() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let run_id = "run-cook-delivery-record";
    let lifecycle_store = store_with_run(&context, run_id);

    let mut record = record_promotion_in_store(
        &lifecycle_store,
        run_id,
        promotion(run_id, "applied", gate("succeeded")),
    )
    .expect("promotion recorded");
    assert_eq!(
        persisted_delivery(&record).state,
        CookDeliveryState::Verified
    );
    let revision = record.revision;
    for _ in 0..2 {
        record = record_cook_finalization_in_store(&lifecycle_store, run_id, published())
            .expect("finalization recorded");
    }
    // Recording the same receipt again commits nothing.
    assert_eq!(record.revision, revision + 1);

    let read = persisted_delivery(&record);
    assert_eq!(read.state, CookDeliveryState::Published);
    assert_eq!(
        recorded_finalization_receipt(&record.metadata),
        Some(&published())
    );
    for key in [
        "latest_promotion",
        "promotions",
        "cook_finalization",
        "cook_recovery_source_checkpoint",
    ] {
        assert!(record.metadata.get(key).is_none(), "{key} is not written");
    }

    // A record of an unknown schema is not trusted.
    let mut foreign = record.metadata.clone();
    foreign["cook_delivery"]["schema"] = json!("homeboy/cook-delivery/v0");
    assert_eq!(CookDelivery::from_run_metadata(&foreign), None);
    assert_eq!(recorded_finalization_receipt(&foreign), None);
}

/// A writer that read the record before a concurrent writer committed must
/// not overwrite that writer's receipt: its commit conflicts, and the retry
/// applies its receipt to the fresh record.
#[test]
fn concurrent_delivery_writers_do_not_lose_an_update() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let run_id = "run-cook-delivery-concurrent";
    let lifecycle_store = store_with_run(&context, run_id);
    let before = lifecycle_store.read_record(run_id).expect("record");

    let mut interleaved = false;
    let mut decisions = 0;
    let record = record_cook_delivery(&lifecycle_store, run_id, |_, draft| {
        decisions += 1;
        if !interleaved {
            interleaved = true;
            // A second writer commits after this one read the record.
            record_cook_finalization_in_store(&lifecycle_store, run_id, published())
                .expect("concurrent finalization recorded");
        }
        draft.artifacts.promotion = Some(promotion(run_id, "applied", gate("succeeded")));
        Some(CookDeliveryCommit::FollowDelivery)
    })
    .expect("promotion recorded");

    assert_eq!(
        decisions, 2,
        "the stale decision conflicted and was retried"
    );
    assert_eq!(record.revision, before.revision + 2);
    assert_eq!(
        recorded_finalization_receipt(&record.metadata),
        Some(&published())
    );
    assert_eq!(
        recorded_promotion_report(&record.metadata)
            .unwrap()
            .expect("promotion")["status"],
        "applied"
    );
    assert_eq!(
        persisted_delivery(&record).state,
        CookDeliveryState::Published
    );

    // Real threads: every writer's receipt survives.
    let writers = ["a", "b", "c", "d"].map(|key| {
        let lifecycle_store = lifecycle_store.clone();
        std::thread::spawn(move || {
            record_cook_delivery(&lifecycle_store, run_id, |record, _| {
                record
                    .ensure_metadata_object()
                    .insert(format!("concurrent_receipt_{key}"), json!(key));
                Some(CookDeliveryCommit::FollowDelivery)
            })
            .expect("concurrent receipt recorded");
        })
    });
    for writer in writers {
        writer.join().expect("writer finished");
    }
    let record = lifecycle_store.read_record(run_id).expect("record");
    for key in ["a", "b", "c", "d"] {
        assert_eq!(record.metadata[format!("concurrent_receipt_{key}")], key);
    }
    assert_eq!(
        persisted_delivery(&record).state,
        CookDeliveryState::Published
    );
}

/// A deferred gate leaves the run recoverable with a `gates_deferred`
/// blocker; a later executed gate replaces it with its verdict and never
/// leaves the run reporting `gates_deferred`.
#[test]
fn a_deferred_then_executed_gate_never_stays_gates_deferred() {
    for (promoted, executed, state, blocker, run_state, cook_status) in [
        (
            "applied",
            "succeeded",
            CookDeliveryState::Verified,
            None,
            AgentTaskRunState::Succeeded,
            None,
        ),
        (
            "gate_failed",
            "failed",
            CookDeliveryState::Blocked,
            Some(CookDeliveryBlockerCode::GateFailed),
            AgentTaskRunState::CandidateRecoverable,
            Some(CookStatus::GateFailed),
        ),
    ] {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let run_id = "run-cook-delivery-deferred";
        let lifecycle_store = store_with_run(&context, run_id);
        lifecycle_store
            .transition_when(run_id, |current| {
                let mut prepared = current.clone();
                set_run_state(&mut prepared, AgentTaskRunState::Succeeded);
                Some(AgentTaskTransition::CommitRecord {
                    prepared: Box::new(prepared),
                })
            })
            .expect("provider succeeded");

        // Promotions and delivery use the canonical `gates_deferred` spelling.
        let deferred = record_promotion_in_store(
            &lifecycle_store,
            run_id,
            promotion(run_id, "gates_deferred", gate("deferred")),
        )
        .expect("deferred promotion recorded");
        let delivery = persisted_delivery(&deferred);
        assert_eq!(
            delivery.blocker_code(),
            Some(CookDeliveryBlockerCode::GatesDeferred)
        );
        assert_eq!(
            deferred.metadata["cook_delivery"]["delivery"]["blocker"]["code"],
            "gates_deferred"
        );
        assert_eq!(deferred.state(), AgentTaskRunState::CandidateRecoverable);
        // The Cook loop recorded its deferred result.
        lifecycle_store
            .mutate_record(run_id, |record| {
                record.ensure_metadata_object().insert(
                    "cook_progress".to_string(),
                    json!({ "phase": "terminal", "terminal_status": "gates_deferred", "terminal_success": false, "exit_code": 1 }),
                );
                true
            })
            .expect("cook result recorded");

        let verified = record_promotion_in_store(
            &lifecycle_store,
            run_id,
            promotion(run_id, promoted, gate(executed)),
        )
        .expect("executed promotion recorded");
        let delivery = persisted_delivery(&verified);
        assert_eq!(delivery.state, state, "{executed}");
        assert_eq!(delivery.blocker_code(), blocker, "{executed}");
        assert_eq!(verified.state(), run_state, "{executed}");
        assert_eq!(delivery.lifecycle().cook_status, cook_status, "{executed}");
        let terminal_status = verified.metadata["cook_progress"]["terminal_status"].as_str();
        assert_ne!(terminal_status, Some("gates_deferred"), "{executed}");
        assert_eq!(
            terminal_status.map(CookStatus::from_status),
            cook_status,
            "{executed}"
        );
    }
}

/// A no-patch gate failure is written with its one canonical spelling and
/// leaves the run recoverable.
#[test]
fn a_no_change_gate_failure_is_recoverable_and_canonically_spelled() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let run_id = "run-cook-delivery-no-change-gate";
    let lifecycle_store = store_with_run(&context, run_id);
    let record = record_promotion_in_store(
        &lifecycle_store,
        run_id,
        promotion(run_id, "no_changes_gate_failed", gate("failed")),
    )
    .expect("promotion recorded");

    let delivery = persisted_delivery(&record);
    assert_eq!(
        delivery.blocker_code(),
        Some(CookDeliveryBlockerCode::GateFailed)
    );
    assert_eq!(
        delivery
            .lifecycle()
            .cook_status
            .as_ref()
            .map(CookStatus::as_str),
        Some("no_changes_gate_failed")
    );
    assert_eq!(record.state(), AgentTaskRunState::CandidateRecoverable);
}

/// One recoverable run state (#15718): a run whose only task ends
/// candidate-recoverable derives `CandidateRecoverable`, Cook delivery's
/// verdict leaves it there, and the candidate stays eligible for recovery in
/// both readings.
#[test]
fn a_candidate_recoverable_run_reads_the_same_before_and_after_delivery() {
    use homeboy_control_plane_contract::{ControlPlaneAction, ControlPlaneActionAvailability};
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let run_id = "run-cook-delivery-one-recoverable-state";
    let lifecycle_store = store_with_run(&context, run_id);
    let plan = test_plan();
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.totals.succeeded = 0;
    aggregate.totals.candidate_recoverable = 1;
    aggregate.outcomes[0].status = crate::agent_task::AgentTaskOutcomeStatus::CandidateRecoverable;
    aggregate.events[0].state = AgentTaskState::CandidateRecoverable;
    let recover_eligibility = |record: &AgentTaskRunRecord| {
        lifecycle_action_eligibility(record, None)
            .actions
            .into_iter()
            .find(|entry| entry.action == ControlPlaneAction::Promote)
            .expect("promote action")
            .availability
    };

    let before = record_run_aggregate_in_store(&lifecycle_store, run_id, &plan, &aggregate)
        .expect("aggregate recorded");
    assert_eq!(aggregate.status(), AgentTaskRunState::CandidateRecoverable);
    assert_eq!(before.state(), AgentTaskRunState::CandidateRecoverable);
    assert_eq!(
        recover_eligibility(&before),
        ControlPlaneActionAvailability::Indeterminate
    );

    let after = record_promotion_in_store(
        &lifecycle_store,
        run_id,
        promotion(run_id, "gate_failed", gate("failed")),
    )
    .expect("delivery verdict recorded");
    assert_eq!(
        persisted_delivery(&after).lifecycle().run_state,
        Some(AgentTaskRunState::CandidateRecoverable)
    );
    assert_eq!(after.state(), AgentTaskRunState::CandidateRecoverable);
    assert_eq!(
        lifecycle_store.read_record(run_id).expect("reread").state(),
        AgentTaskRunState::CandidateRecoverable
    );
    assert_eq!(
        recover_eligibility(&after),
        ControlPlaneActionAvailability::Indeterminate
    );
}
