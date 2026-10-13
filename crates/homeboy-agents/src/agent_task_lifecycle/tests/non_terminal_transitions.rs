//! Compare-and-swap non-terminal writes (#15718, step 4).
//!
//! A caller-held snapshot used to be committed wholesale. The SQL guard only
//! refused `running` over a terminal row, so a stale writer silently replaced
//! any non-terminal transition committed after it read the record: a Lab
//! handoff binding, a cancellation request, provider progress.

use super::stub_lab_offload_submission;
use crate::agent_task_lifecycle::{
    record_detached_lab_run_in_store, record_lab_offload_planned_with_submission_in_store,
    recover_transport_proxy_in_store, rewrite_record_for_test_in_store, AgentTaskLabHandoffState,
    AgentTaskLifecycleStore, AgentTaskRunState, DetachedLabRunRecord, LabOffloadProxyPlan,
    RunnerAuthority, RunnerContinuationProvider, RunnerContinuationTestGuard,
    AGENT_TASK_TRANSITION_CONFLICT,
};
use homeboy_core::api_jobs::{Job, RunnerJobLogSnapshot};
use homeboy_core::{Error, Result};
use std::sync::Mutex;

const RUN_ID: &str = "stale-proxy-recovery";
const RUNNER_ID: &str = "homeboy-lab";
const RUNNER_JOB_ID: &str = "job-accepted-during-recovery";

fn command() -> Vec<String> {
    vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ]
}

/// The runner daemon accepts the handoff while transport-proxy recovery is
/// asking the provider about the runner: another controller process binds the
/// acceptance in exactly that window.
struct AcceptsDuringAuthorityProbe {
    store: AgentTaskLifecycleStore,
    accepted: Mutex<bool>,
}

impl RunnerContinuationProvider for AcceptsDuringAuthorityProbe {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<RunnerJobLogSnapshot> {
        Err(Error::internal_unexpected("not used by this fixture"))
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        false
    }

    fn runner_authority(&self, _runner_id: &str) -> RunnerAuthority {
        let mut accepted = self.accepted.lock().expect("acceptance flag");
        if !*accepted {
            let command = command();
            record_detached_lab_run_in_store(
                &self.store,
                DetachedLabRunRecord {
                    run_id: RUN_ID,
                    runner_id: RUNNER_ID,
                    runner_job_id: RUNNER_JOB_ID,
                    remote_workspace: "/runner/workspace/repo",
                    remote_command: &command,
                },
            )
            .expect("concurrent acceptance binds the pending handoff");
            *accepted = true;
        }
        RunnerAuthority::Unknown
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        Err(Error::internal_unexpected("not used by this fixture"))
    }

    fn submit_runner_api_request(
        &self,
        _runner_id: &str,
        _submission: homeboy_runner_contract::RunnerApiSubmitRequest,
    ) -> Result<Job> {
        Err(Error::internal_unexpected("not used by this fixture"))
    }
}

/// Transport-proxy recovery reads the unbound proxy, asks the provider whether
/// the runner is configured, and then commits its `reconnect required`
/// annotation. The runner accepts the handoff in between. Committing the
/// recovery's snapshot used to revert the run to `Queued` with no runner job:
/// the accepted handoff, the job identity and the running state were lost, and
/// only a later expiry or reserved-job reconciliation could notice.
#[test]
fn stale_proxy_recovery_cannot_unbind_a_concurrent_lab_handoff_acceptance() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let command = command();
    record_lab_offload_planned_with_submission_in_store(
        &store,
        LabOffloadProxyPlan {
            run_id: RUN_ID,
            runner_id: RUNNER_ID,
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("controller proxy planned before acceptance");
    // The stub admission pins no real controller runtime; recovery validates
    // the pin, so the fixture carries none.
    rewrite_record_for_test_in_store(&store, RUN_ID, |record| {
        if let Some(metadata) = record.metadata.as_object_mut() {
            metadata.remove(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY);
        }
    })
    .expect("drop the stub runtime pin");
    let planned = store.read_record(RUN_ID).expect("planned proxy");
    assert_eq!(planned.state, AgentTaskRunState::Queued);
    assert!(planned.runner_job_id().is_none());

    let _provider = RunnerContinuationTestGuard::install(Box::new(AcceptsDuringAuthorityProbe {
        store: AgentTaskLifecycleStore::new(context.path_roots()),
        accepted: Mutex::new(false),
    }));
    let recovery = recover_transport_proxy_in_store(&store, RUN_ID);

    let stored = store.read_record(RUN_ID).expect("stored record");
    assert_eq!(
        stored.state,
        AgentTaskRunState::Running,
        "lost update: a stale recovery snapshot reverted the accepted handoff's running state"
    );
    assert_eq!(stored.runner_job_id(), Some(RUNNER_JOB_ID));
    assert_eq!(
        stored.lab_handoff.as_ref().map(|handoff| handoff.state),
        Some(AgentTaskLabHandoffState::Accepted)
    );
    // The recovery decided from a record that no longer exists, so it commits
    // nothing and reports the conflict.
    let error = recovery.expect_err("stale recovery is refused");
    assert_eq!(
        error.details["reason_code"],
        serde_json::json!(AGENT_TASK_TRANSITION_CONFLICT)
    );
}

// --- The fence itself -------------------------------------------------------

fn planned_proxy(store: &AgentTaskLifecycleStore) {
    let command = command();
    record_lab_offload_planned_with_submission_in_store(
        store,
        LabOffloadProxyPlan {
            run_id: RUN_ID,
            runner_id: RUNNER_ID,
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("controller proxy planned");
}

/// A snapshot committed after another writer moved the run is refused and
/// writes nothing; the copy read at the current revision commits.
#[test]
fn a_stale_annotation_is_refused_and_writes_nothing() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    planned_proxy(&store);
    let mut stale = store.read_record(RUN_ID).expect("first reader");
    store
        .mutate_record(RUN_ID, |record| {
            record.metadata["concurrent_progress"] = serde_json::json!("kept");
            true
        })
        .expect("concurrent writer commits");
    let concurrent = store
        .read_record(RUN_ID)
        .expect("after the concurrent write");

    stale.metadata["stale_annotation"] = serde_json::json!(true);
    let error = store
        .commit_record(&mut stale)
        .expect_err("a stale annotation conflicts");
    assert_eq!(
        error.details["reason_code"],
        serde_json::json!(AGENT_TASK_TRANSITION_CONFLICT)
    );
    let stored = store.read_record(RUN_ID).expect("stored");
    assert_eq!(stored, concurrent, "a refused commit writes nothing");

    let mut fresh = stored;
    fresh.metadata["fresh_annotation"] = serde_json::json!(true);
    store
        .commit_record(&mut fresh)
        .expect("a current annotation commits");
    let stored = store.read_record(RUN_ID).expect("stored");
    assert_eq!(stored.metadata["concurrent_progress"], "kept");
    assert_eq!(stored.metadata["fresh_annotation"], true);
    assert_eq!(
        stored.revision, fresh.revision,
        "the caller's copy is synced"
    );
}

/// State and ownership move only through typed transitions: an annotation
/// that changes the state or the Lab handoff is refused, and the typed
/// transition validates the state it leaves.
#[test]
fn annotations_cannot_change_state_or_ownership() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    planned_proxy(&store);

    let mut running = store.read_record(RUN_ID).expect("queued proxy");
    crate::agent_task_lifecycle::set_run_state(&mut running, AgentTaskRunState::Running);
    let error = store
        .commit_record(&mut running)
        .expect_err("an annotation cannot start the run");
    assert!(error.message.contains("the run state"), "{}", error.message);

    let mut handoff = store.read_record(RUN_ID).expect("queued proxy");
    handoff.lab_handoff = Some(crate::agent_task_lifecycle::AgentTaskLabHandoff::pending(
        RUNNER_ID,
        "2026-10-12T00:00:00Z".to_string(),
        "2999-01-01T00:00:00Z".to_string(),
    ));
    let error = store
        .commit_record(&mut handoff.clone())
        .expect_err("an annotation cannot publish a handoff");
    assert!(
        error.message.contains("the Lab handoff"),
        "{}",
        error.message
    );
    store
        .advance_record(
            &mut handoff,
            crate::agent_task_lifecycle::AgentTaskAdvance::PendLabHandoff,
        )
        .expect("the typed transition publishes it");

    let mut requeued = store.read_record(RUN_ID).expect("pending handoff");
    let error = store
        .advance_record(
            &mut requeued,
            crate::agent_task_lifecycle::AgentTaskAdvance::BindLabHandoff,
        )
        .expect_err("binding requires a running, accepted runner job");
    assert!(
        error.message.contains("binding a Lab handoff"),
        "{}",
        error.message
    );

    let stored = store.read_record(RUN_ID).expect("stored");
    assert_eq!(stored.state, AgentTaskRunState::Queued);
    assert_eq!(
        stored.lab_handoff.as_ref().map(|handoff| handoff.state),
        Some(AgentTaskLabHandoffState::Pending)
    );
}
