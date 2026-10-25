use super::*;
use homeboy_core::test_support::with_isolated_home;
use serde_json::json;

fn record(id: &str, state: &str, submitted_at: &str, metadata: Value) -> AgentTaskRunRecord {
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1",
        "run_id": id,
        "plan_id": "plan",
        "state": state,
        "submitted_at": submitted_at,
        "plan_path": "plan.json",
        "metadata": metadata,
    }))
    .expect("record");
    let state = match state {
        "queued" => agent_task_lifecycle::AgentTaskRunState::Queued,
        "running" => agent_task_lifecycle::AgentTaskRunState::Running,
        "succeeded" => agent_task_lifecycle::AgentTaskRunState::Succeeded,
        _ => unreachable!(),
    };
    agent_task_lifecycle::set_run_state(&mut record, state);
    record
}

#[test]
fn admission_loads_terminal_cook_parent_outside_active_set() {
    with_isolated_home(|_| {
        let store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment().unwrap();
        store
            .write_record(&record(
                "parent",
                "succeeded",
                "2000-01-01T00:00:00Z",
                json!({}),
            ))
            .unwrap();
        store
            .write_record(&record(
                "attempt",
                "running",
                "2000-01-01T00:00:00Z",
                json!({
                    "cook_id": "parent", "runner_id": "lab", "runner_job_id": "old-job"
                }),
            ))
            .unwrap();
        let (active, _) = store.read_active_records().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].run_id, "attempt");

        let admission = AgentTaskControllerUpgradeAdmissionProvider
            .controller_upgrade_admission()
            .unwrap();
        assert!(admission.blockers.is_empty(), "{:?}", admission.blockers);
    });
}

#[test]
fn admission_blocks_nonterminal_record_older_than_display_window() {
    with_isolated_home(|_| {
        let store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment().unwrap();
        store
            .write_record(&record(
                "old-active",
                "running",
                "2000-01-01T00:00:00Z",
                json!({"runner_id": "lab", "runner_job_id": "old-job"}),
            ))
            .unwrap();
        for index in 0..1001 {
            store
                .write_record(&record(
                    &format!("terminal-{index}"),
                    "succeeded",
                    "2026-01-01T00:00:00Z",
                    json!({}),
                ))
                .unwrap();
        }
        let (bounded, _) = store.read_records_with_health_bounded(1000).unwrap();
        assert!(!bounded.iter().any(|record| record.run_id == "old-active"));

        let admission = AgentTaskControllerUpgradeAdmissionProvider
            .controller_upgrade_admission()
            .unwrap();
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].run_id, "old-active");
    });
}
