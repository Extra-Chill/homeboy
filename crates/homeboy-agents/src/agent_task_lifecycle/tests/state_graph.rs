//! The run state transition graph is enforced at the write choke point
//! (#15718, step 4b).
//!
//! Every production write is fenced on the revision it read, but a mutation
//! closure or a prepared record could still set any state from any state.
//! Nothing checked that the edge was legal: a finished run could be committed
//! back to `Running`, and a cancelled run re-projected as a success.

use super::test_plan;
use crate::agent_task_lifecycle::{
    set_run_state, AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
    AgentTaskTerminalOutcome, AgentTaskTransition, AGENT_TASK_ILLEGAL_TRANSITION,
};
use homeboy_core::run_lifecycle_record::{RunExecutionState, RunLifecycleRecord};
use serde_json::json;

const RUN_ID: &str = "state-graph-run";

fn finished(
    context: &homeboy_core::test_support::HermeticTestContext,
    outcome: AgentTaskTerminalOutcome,
) -> AgentTaskLifecycleStore {
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    store
        .write_controller_plan(RUN_ID, &plan)
        .expect("write controller plan");
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": crate::agent_task_lifecycle::records::schemas::RUN,
        "run_id": RUN_ID,
        "plan_id": plan.plan_id,
        "state": "queued",
        "submitted_at": "2026-10-20T00:00:00Z",
        "plan_path": store.controller_plan_path(RUN_ID).display().to_string(),
    }))
    .expect("decode minimal run record");
    record.lifecycle = RunLifecycleRecord::with_execution_state(RunExecutionState::Queued);
    set_run_state(&mut record, AgentTaskRunState::Running);
    store.write_record(&record).expect("commit running record");
    let running = store.read_record(RUN_ID).expect("running");
    store
        .transition(
            RUN_ID,
            running.revision,
            AgentTaskTransition::Terminate {
                outcome,
                metadata: serde_json::Map::new(),
            },
        )
        .expect("finish the run");
    store
}

fn assert_refused(
    store: &AgentTaskLifecycleStore,
    committed: homeboy_core::Result<Option<AgentTaskRunRecord>>,
    before: &AgentTaskRunRecord,
) {
    let read = store.read_record(RUN_ID).expect("stored record");
    let row = store
        .open_observation_readonly()
        .unwrap()
        .get_run(RUN_ID)
        .unwrap()
        .expect("stored row");
    let error = match committed {
        Ok(record) => panic!(
            "an illegal {:?} -> {:?} commit was reported committed as {:?} (stored {:?}, row `{}`)",
            before.state(),
            record.as_ref().map(|record| record.state()),
            record.as_ref().map(|record| record.revision),
            read.state(),
            row.status,
        ),
        Err(error) => error,
    };
    assert_eq!(
        error.details["reason_code"],
        json!(AGENT_TASK_ILLEGAL_TRANSITION),
        "{error:?}"
    );
    assert_eq!(error.details["from_state"], json!(before.state()));
    assert_eq!(
        read.state(),
        before.state(),
        "a refused commit writes nothing"
    );
    assert_eq!(
        read.revision, before.revision,
        "a refused commit writes nothing"
    );
}

/// A mutation decided from a finished record cannot resume it in place. On
/// main the SQL terminal-row guard dropped the write and `mutate_record`
/// reported the stored `Succeeded` record as if the resumption committed.
#[test]
fn a_mutation_cannot_resume_a_finished_run_in_place() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = finished(&context, AgentTaskTerminalOutcome::Succeeded);
    let before = store.read_record(RUN_ID).unwrap();
    assert_eq!(before.state(), AgentTaskRunState::Succeeded);

    let committed = store.mutate_record(RUN_ID, |record| {
        set_run_state(record, AgentTaskRunState::Running);
        true
    });

    assert_refused(&store, committed, &before);
}

/// A cancellation is final. On main a mutation could re-project a cancelled
/// run as `Succeeded`: the terminal-row guard admits any terminal status, so
/// the run's immutable `run.cancelled` receipt and its stored state disagreed.
#[test]
fn a_cancelled_run_cannot_be_reprojected_as_a_success() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = finished(&context, AgentTaskTerminalOutcome::Cancelled);
    let before = store.read_record(RUN_ID).unwrap();
    assert_eq!(before.state(), AgentTaskRunState::Cancelled);

    let committed = store.mutate_record(RUN_ID, |record| {
        if let Some(metadata) = record.metadata.as_object_mut() {
            metadata.remove("cancellation_provenance");
        }
        set_run_state(record, AgentTaskRunState::Succeeded);
        true
    });

    assert_refused(&store, committed, &before);
}

/// The same edge through a typed terminal commit: `CommitRecord` checked only
/// that its target was terminal, not that the stored run could reach it.
#[test]
fn a_terminal_record_commit_cannot_leave_a_cancellation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = finished(&context, AgentTaskTerminalOutcome::Cancelled);
    let before = store.read_record(RUN_ID).unwrap();

    let mut prepared = before.clone();
    if let Some(metadata) = prepared.metadata.as_object_mut() {
        metadata.remove("cancellation_provenance");
    }
    set_run_state(&mut prepared, AgentTaskRunState::Failed);
    let committed = store
        .transition(
            RUN_ID,
            before.revision,
            AgentTaskTransition::CommitRecord {
                prepared: Box::new(prepared),
            },
        )
        .map(Some)
        .map_err(homeboy_core::Error::from);

    assert_refused(&store, committed, &before);
}

/// Legal edges still commit through the same path.
#[test]
fn a_finished_run_is_still_reprojected_from_later_evidence() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = finished(&context, AgentTaskTerminalOutcome::Failed);

    let reprojected = store
        .mutate_record(RUN_ID, |record| {
            set_run_state(record, AgentTaskRunState::Succeeded);
            true
        })
        .expect("a failure may be re-projected from later evidence")
        .expect("written");
    assert_eq!(reprojected.state(), AgentTaskRunState::Succeeded);
}
