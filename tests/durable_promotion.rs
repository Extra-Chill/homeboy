use homeboy_agents::agent_task_service::{
    recorded_promotion_history, recorded_promotion_report, recorded_promotion_report_typed,
};
use homeboy_agents::agent_tasks::lifecycle::{
    record_completed_run, record_promotion, run_id_for_aggregate_path, status,
};
use homeboy_agents::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::test_support::{HomeGuard, TestBinary};
use serde_json::json;

#[test]
fn aggregate_promotion_persists_a_restart_safe_idempotent_finalization_proof() {
    let _home = HomeGuard::with_controller_runtime(TestBinary::HomeboyFixture);
    let run_id = "cook-8399-attempt-1";
    let plan: AgentTaskPlan = serde_json::from_value(json!({
        "plan_id": "issue-8399",
        "tasks": [{
            "schema": "homeboy/agent-task-request/v1",
            "task_id": "task-8399",
            "executor": { "backend": "fixture", "selector": "fixture" },
            "instructions": "produce a patch",
            "workspace": {},
            "policy": {},
            "limits": {}
        }]
    }))
    .expect("plan");
    let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-aggregate/v1",
        "plan_id": "issue-8399",
        "status": "succeeded",
        "totals": {
            "queued": 0, "running": 0, "blocked": 0, "skipped": 0,
            "succeeded": 1, "candidate_recoverable": 0, "recoverable_candidates": 0,
            "failed": 0, "cancelled": 0, "timed_out": 0
        },
        "outcomes": [{
            "schema": "homeboy/agent-task-outcome/v1",
            "task_id": "task-8399", "status": "succeeded", "artifacts": []
        }]
    }))
    .expect("aggregate");

    let completed = record_completed_run(&plan, &aggregate, Some(run_id)).expect("completed run");
    let aggregate_path = std::path::PathBuf::from(
        completed
            .aggregate_path
            .as_deref()
            .expect("durable aggregate path"),
    );
    assert_eq!(
        run_id_for_aggregate_path(&aggregate_path).expect("aggregate owner"),
        Some(run_id.to_string())
    );

    let promotion = json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "applied",
        "source": { "kind": "aggregate", "task_id": "task-8399", "run_id": run_id },
        "to_worktree": "homeboy@fix-8399-durable-cook-promotion-state",
        "target": {
            "worktree": "homeboy@fix-8399-durable-cook-promotion-state",
            "path": "/recreated-target"
        },
        "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch" },
        "changed_files": ["src/lib.rs"],
        "gate_results": [{ "id": "test", "name": "cargo test", "kind": "command", "status": "passed" }],
        "operator_notification": { "status": "completed", "message": "gates passed" }
    });
    record_promotion(run_id, promotion.clone()).expect("applied promotion persisted");
    record_promotion(run_id, promotion.clone()).expect("identical promotion is idempotent");

    let restarted = status(run_id).expect("reload durable run");
    assert_eq!(
        recorded_promotion_report(&restarted.metadata).expect("readable promotion"),
        Some(promotion)
    );
    assert_eq!(
        recorded_promotion_history(&restarted.metadata)
            .expect("readable history")
            .expect("promotion history")
            .len(),
        1
    );
}

/// A report above the metadata payload threshold is stored as a
/// content-addressed reference. Every reader must get the report back, not
/// the reference (#16664: finalization parsed the reference and refused to
/// publish a gated candidate).
#[test]
fn an_externalized_promotion_report_reads_back_as_the_report() {
    let _home = HomeGuard::with_controller_runtime(TestBinary::HomeboyFixture);
    let run_id = "cook-16664-attempt-1";
    let (plan, aggregate) = plan_and_aggregate("issue-16664", "task-16664");
    record_completed_run(&plan, &aggregate, Some(run_id)).expect("completed run");

    let changed_files: Vec<String> = (0..2_000)
        .map(|index| format!("crates/large/src/module_{index:04}.rs"))
        .collect();
    let promotion = json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "applied",
        "source": { "kind": "aggregate", "task_id": "task-16664", "run_id": run_id },
        "to_worktree": "homeboy@fix-16664",
        "target": { "worktree": "homeboy@fix-16664", "path": "/target" },
        "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch" },
        "changed_files": changed_files,
        "gate_results": [{ "id": "check", "name": "cargo check", "kind": "command", "status": "passed" }],
        "operator_notification": { "status": "completed", "message": "gates passed" }
    });
    assert!(
        serde_json::to_vec(&promotion).unwrap().len()
            > homeboy_core::observation::store::metadata_payloads::METADATA_PAYLOAD_THRESHOLD
    );
    record_promotion(run_id, promotion.clone()).expect("applied promotion persisted");

    let restarted = status(run_id).expect("reload durable run");
    assert!(
        restarted
            .metadata
            .pointer("/cook_delivery/artifacts/promotion/metadata_payload")
            .is_some(),
        "the stored report must be externalized for this test to mean anything"
    );
    assert_eq!(
        recorded_promotion_report(&restarted.metadata).expect("resolved promotion"),
        Some(promotion.clone())
    );
    assert_eq!(
        recorded_promotion_history(&restarted.metadata).expect("resolved history"),
        Some(vec![promotion])
    );
    let typed = recorded_promotion_report_typed(&restarted.metadata)
        .expect("typed promotion")
        .expect("promotion present");
    assert_eq!(typed.source.run_id.as_deref(), Some(run_id));
    assert_eq!(typed.changed_files.len(), 2_000);
}

fn plan_and_aggregate(plan_id: &str, task_id: &str) -> (AgentTaskPlan, AgentTaskAggregate) {
    let plan = serde_json::from_value(json!({
        "plan_id": plan_id,
        "tasks": [{
            "schema": "homeboy/agent-task-request/v1",
            "task_id": task_id,
            "executor": { "backend": "fixture", "selector": "fixture" },
            "instructions": "produce a patch",
            "workspace": {},
            "policy": {},
            "limits": {}
        }]
    }))
    .expect("plan");
    let aggregate = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-aggregate/v1",
        "plan_id": plan_id,
        "status": "succeeded",
        "totals": {
            "queued": 0, "running": 0, "blocked": 0, "skipped": 0,
            "succeeded": 1, "candidate_recoverable": 0, "recoverable_candidates": 0,
            "failed": 0, "cancelled": 0, "timed_out": 0
        },
        "outcomes": [{
            "schema": "homeboy/agent-task-outcome/v1",
            "task_id": task_id, "status": "succeeded", "artifacts": []
        }]
    }))
    .expect("aggregate");
    (plan, aggregate)
}
