use super::super::cook_run_state::CookSettlement;
use super::super::AgentTaskRunRecord;
use crate::agent_task_service::CookContinuationState as C;
use homeboy_lifecycle_contract::RunExecutionState as R;

#[test]
fn continuation_settlement_matrix() {
    for terminal in [false, true] {
        for driver in [false, true] {
            for continuation in [C::Absent, C::Pending, C::Claimed, C::Failed, C::Completed] {
                for runner in [false, true] {
                    for outcome in [R::Succeeded, R::CandidateRecoverable, R::Failed] {
                        let state = if terminal { outcome } else { R::Running };
                        let record: AgentTaskRunRecord = serde_json::from_value(serde_json::json!({
                            "schema": "homeboy/agent-task-run/v1", "run_id": "run", "plan_id": "plan",
                            "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
                            "metadata": if runner { serde_json::json!({"runner_id": "runner"}) } else { serde_json::json!({}) },
                        })).unwrap();
                        let settlement = CookSettlement(continuation);
                        let expected = terminal
                            && !driver
                            && match continuation {
                                C::Pending | C::Claimed => false,
                                C::Absent => !runner || outcome == R::Failed,
                                C::Failed | C::Completed => true,
                            };
                        assert_eq!(settlement.is_settled(&record, driver), expected,
                            "terminal={terminal}, driver={driver}, continuation={continuation:?}, runner={runner}, outcome={outcome:?}");
                    }
                }
            }
        }
    }
}
