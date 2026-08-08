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
    assert_eq!(restarted.metadata["latest_promotion"], promotion);
    assert_eq!(
        restarted.metadata["promotions"]
            .as_array()
            .expect("promotion history")
            .len(),
        1
    );
}
