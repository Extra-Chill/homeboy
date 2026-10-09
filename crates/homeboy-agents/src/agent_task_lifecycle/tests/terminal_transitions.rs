//! Compare-and-swap terminal transitions (#15718, step 2a).
//!
//! The lifecycle store used to rewrite the whole snapshot on every write. The
//! only guard was SQL refusing a `running` status over a terminal row, so a
//! stale writer that was *itself* terminal replaced a concurrent terminal
//! decision wholesale.

use super::{succeeded_aggregate, test_plan};
use crate::agent_task_lifecycle::{
    apply_aggregate_transition_in_store, set_run_state, AgentTaskAggregateTransition,
    AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState, AgentTaskTerminalOutcome,
    AgentTaskTransition, AgentTaskTransitionError,
};
use homeboy_core::run_lifecycle_record::{RunExecutionState, RunLifecycleRecord};
use serde_json::json;

const RUN_ID: &str = "stale-terminal-writer";

fn running_record(store: &AgentTaskLifecycleStore) -> AgentTaskRunRecord {
    let plan = test_plan();
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": crate::agent_task_lifecycle::records::schemas::RUN,
        "run_id": RUN_ID,
        "plan_id": plan.plan_id,
        "state": "queued",
        "submitted_at": "2026-10-09T00:00:00Z",
        "plan_path": store.controller_plan_path(RUN_ID).display().to_string(),
        "metadata": { "store_marker": "base" },
    }))
    .expect("decode minimal run record");
    record.lifecycle = RunLifecycleRecord::with_execution_state(RunExecutionState::Queued);
    set_run_state(&mut record, AgentTaskRunState::Running);
    record
}

/// Writer A terminalizes the run as `Failed` with a controller failure. Writer
/// B read the same running snapshot first, then projects a successful terminal
/// aggregate from it. Both writes carry a terminal status, so the old SQL guard
/// let B silently replace A's state and failure evidence.
#[test]
fn stale_terminal_projection_cannot_overwrite_a_concurrent_terminal_failure() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    store
        .write_controller_plan(RUN_ID, &plan)
        .expect("write controller plan");
    store
        .write_record(&running_record(&store))
        .expect("commit running record");

    // Both writers observe the same running snapshot.
    let mut writer_b = store.read_record(RUN_ID).expect("writer B reads");
    assert_eq!(writer_b.state, AgentTaskRunState::Running);

    // Writer A terminalizes first.
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["cook_controller_failure"] = json!({
                "reason": "writer A: provider exited with a controller failure",
            });
            set_run_state(record, AgentTaskRunState::Failed);
            record.updated_at = Some("2026-10-09T00:00:01Z".to_string());
            true
        })
        .expect("writer A terminalizes")
        .expect("writer A wrote");

    // Writer B projects its terminal aggregate from the stale snapshot.
    let aggregate = succeeded_aggregate(&plan);
    let outcome = apply_aggregate_transition_in_store(
        &store,
        AgentTaskAggregateTransition {
            record: &mut writer_b,
            plan: &plan,
            aggregate: &aggregate,
        },
    );

    let stored = store.read_record(RUN_ID).expect("read settled record");
    assert_eq!(
        stored.state,
        AgentTaskRunState::Failed,
        "lost update: a stale terminal writer replaced writer A's terminal state"
    );
    assert_eq!(
        stored.metadata["cook_controller_failure"]["reason"],
        "writer A: provider exited with a controller failure",
        "lost update: writer A's terminal failure evidence was erased"
    );
    let error = outcome.expect_err("the stale terminal projection must be refused");
    assert!(
        format!("{error:?}").contains("agent_task_transition_conflict"),
        "stale writer must surface a typed transition conflict: {error:?}"
    );
}

fn seeded(context: &homeboy_core::test_support::HermeticTestContext) -> AgentTaskLifecycleStore {
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    store
        .write_controller_plan(RUN_ID, &test_plan())
        .expect("write controller plan");
    store
        .write_record(&running_record(&store))
        .expect("commit running record");
    store
}

fn failed_with(reason: &str) -> AgentTaskTransition {
    let mut metadata = serde_json::Map::new();
    metadata.insert("terminal_reason".to_string(), json!(reason));
    AgentTaskTransition::Terminate {
        outcome: AgentTaskTerminalOutcome::Failed,
        metadata,
    }
}

fn transition_events(store: &AgentTaskLifecycleStore) -> Vec<serde_json::Value> {
    let run = homeboy_control_plane_contract::RunId::new(RUN_ID).unwrap();
    store
        .open_observation_initialized()
        .unwrap()
        .control_plane_event_stream(&run)
        .unwrap()
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event.kind == "run.transitioned")
        .map(|event| event.data)
        .collect()
}

#[test]
fn every_write_bumps_the_store_owned_revision() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    let first = store.read_record(RUN_ID).unwrap();
    assert_eq!(first.revision, 1);
    // A legacy whole-record write bumps too, whatever revision it carried.
    let mut stale = first.clone();
    stale.revision = 41;
    stale.metadata["progress"] = json!("legacy write");
    store.write_record(&stale).unwrap();
    assert_eq!(store.read_record(RUN_ID).unwrap().revision, 2);
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["progress"] = json!("mutation");
            true
        })
        .unwrap();
    assert_eq!(store.read_record(RUN_ID).unwrap().revision, 3);
}

#[test]
fn transition_commits_at_the_expected_revision_and_records_one_event() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    let current = store.read_record(RUN_ID).unwrap();

    let committed = store
        .transition(RUN_ID, current.revision, failed_with("provider exited"))
        .expect("current revision commits");

    assert_eq!(committed.state, AgentTaskRunState::Failed);
    assert_eq!(committed.revision, current.revision + 1);
    assert_eq!(committed.metadata["terminal_reason"], "provider exited");
    assert_eq!(committed.metadata["store_marker"], "base");
    assert_eq!(
        committed.lifecycle.execution.state,
        RunExecutionState::Failed
    );
    let events = transition_events(&store);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["transition"], "terminate");
    assert_eq!(events[0]["from"], "running");
    assert_eq!(events[0]["to"], "failed");
    assert_eq!(events[0]["revision"], committed.revision);
    // The transition is an audit record, not telemetry: it is kept outside the
    // bounded progress window.
    let run = homeboy_control_plane_contract::RunId::new(RUN_ID).unwrap();
    let durable = store
        .open_observation_initialized()
        .unwrap()
        .durable_control_plane_event(&run, "run.transitioned")
        .unwrap()
        .expect("run.transitioned outlives progress-event pruning");
    assert_eq!(durable.data["revision"], committed.revision);
}

#[test]
fn stale_transition_is_a_typed_conflict_and_writes_nothing() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    let stale = store.read_record(RUN_ID).unwrap();
    store
        .transition(RUN_ID, stale.revision, failed_with("writer A"))
        .expect("writer A commits");
    let winner = store.read_record(RUN_ID).unwrap();

    let error = store
        .transition(
            RUN_ID,
            stale.revision,
            AgentTaskTransition::Terminate {
                outcome: AgentTaskTerminalOutcome::Succeeded,
                metadata: serde_json::Map::new(),
            },
        )
        .expect_err("writer B decided from a stale revision");
    match error {
        AgentTaskTransitionError::Conflict(conflict) => {
            assert_eq!(conflict.expected_revision, stale.revision);
            assert_eq!(conflict.actual_revision, winner.revision);
            assert_eq!(conflict.actual_state, Some(AgentTaskRunState::Failed));
            assert_eq!(conflict.transition, "terminate");
        }
        other => panic!("expected a transition conflict, got {other:?}"),
    }
    let stored = store.read_record(RUN_ID).unwrap();
    assert_eq!(stored, winner, "a refused transition must write nothing");
    assert_eq!(transition_events(&store).len(), 1);
}

#[test]
fn a_write_by_an_older_binary_that_drops_the_revision_is_detected() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["progress"] = json!("second write");
            true
        })
        .unwrap();
    let holder = store.read_record(RUN_ID).unwrap();
    assert_eq!(holder.revision, 2);

    // An older binary round-trips the snapshot without the field it does not
    // know, so the stored revision disappears and reads as 0.
    let observation = store.open_observation_initialized().unwrap();
    let mut row = observation.get_run(RUN_ID).unwrap().unwrap();
    row.metadata_json["agent_task_run"]
        .as_object_mut()
        .unwrap()
        .remove("revision");
    row.metadata_json["agent_task_run"]["metadata"]["progress"] = json!("older binary");
    observation
        .upsert_imported_run_preserving_terminal(&row)
        .unwrap();
    assert_eq!(store.read_record(RUN_ID).unwrap().revision, 0);

    let error = store
        .transition(RUN_ID, holder.revision, failed_with("stale holder"))
        .expect_err("the older binary's write moved the record");
    assert!(matches!(error, AgentTaskTransitionError::Conflict(_)));
    let stored = store.read_record(RUN_ID).unwrap();
    assert_eq!(stored.state, AgentTaskRunState::Running);
    assert_eq!(stored.metadata["progress"], "older binary");

    // A writer that re-reads sees revision 0 and commits on top of it.
    let committed = store
        .transition(RUN_ID, 0, failed_with("fresh holder"))
        .expect("re-read revision commits");
    assert_eq!(committed.revision, 1);
}

#[test]
fn transition_when_redecides_from_the_fresh_record_and_skips_a_terminal_run() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    let committed = store
        .transition_when(RUN_ID, |record| {
            (!record.state.is_terminal()).then(|| failed_with("first"))
        })
        .unwrap()
        .expect("live run terminalizes");
    assert_eq!(committed.state, AgentTaskRunState::Failed);
    let second = store
        .transition_when(RUN_ID, |record| {
            (!record.state.is_terminal()).then(|| failed_with("second"))
        })
        .unwrap();
    assert!(second.is_none(), "a terminal run is a no-op");
    assert_eq!(
        store.read_record(RUN_ID).unwrap().metadata["terminal_reason"],
        "first"
    );
}

#[test]
fn a_projection_rebases_over_live_writes_but_not_over_another_terminal_state() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = seeded(&context);
    let plan = test_plan();
    let aggregate = succeeded_aggregate(&plan);

    // A non-terminal write after the snapshot: the projection still commits.
    let mut snapshot = store.read_record(RUN_ID).unwrap();
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["progress"] = json!("heartbeat");
            true
        })
        .unwrap();
    apply_aggregate_transition_in_store(
        &store,
        AgentTaskAggregateTransition {
            record: &mut snapshot,
            plan: &plan,
            aggregate: &aggregate,
        },
    )
    .expect("live intervening write rebases");
    assert_eq!(snapshot.state, AgentTaskRunState::Succeeded);

    // A re-projection decided from the same terminal state also commits.
    let mut terminal = store.read_record(RUN_ID).unwrap();
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["progress"] = json!("terminal evidence");
            true
        })
        .unwrap();
    apply_aggregate_transition_in_store(
        &store,
        AgentTaskAggregateTransition {
            record: &mut terminal,
            plan: &plan,
            aggregate: &aggregate,
        },
    )
    .expect("same-terminal re-projection rebases");
    let events = transition_events(&store);
    assert!(events
        .iter()
        .all(|event| event["transition"] == "project_aggregate"));
    assert_eq!(events.len(), 2, "{events:?}");
}
