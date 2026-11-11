//! A record task's state is a function of its outcome when it has one (#15718).

use super::*;

fn record_task_state(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
) -> AgentTaskState {
    store
        .read_record(run_id)
        .expect("read record")
        .tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .expect("record task")
        .state()
}

#[test]
fn stale_task_event_does_not_override_terminal_outcome() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "stale-task-event";
    let mut plan = test_plan();
    let mut task_b = plan.tasks[0].clone();
    task_b.task_id = "task-b".to_string();
    plan.tasks.push(task_b);
    store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submit run");
    let mut aggregate = succeeded_aggregate(&plan);
    // The last event for `task-a` contradicts its succeeded outcome; `task-b`
    // has no outcome yet, so its event is the only state it has.
    for (task_id, state) in [
        ("task-a", AgentTaskState::Failed),
        ("task-b", AgentTaskState::Running),
    ] {
        aggregate.events.push(AgentTaskProgressEvent {
            task_id: task_id.to_string(),
            state,
            attempt: 1,
            message: None,
        });
    }
    record_run_aggregate_in_store(&store, run_id, &plan, &aggregate).expect("record aggregate");

    assert_eq!(
        record_task_state(&store, run_id, "task-a"),
        AgentTaskState::Succeeded
    );
    assert_eq!(
        record_task_state(&store, run_id, "task-b"),
        AgentTaskState::Running
    );
}
