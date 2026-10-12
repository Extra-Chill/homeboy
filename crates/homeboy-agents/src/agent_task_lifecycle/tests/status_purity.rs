//! `agent-task status` is a pure projection (#13697, #15718 step 3).
//!
//! The status read used to share its body with the reconciler, so reading a
//! run could re-project runner snapshots, terminalize it, interrupt an
//! adoption, and enqueue a Cook continuation that the daemon then executed
//! (#15668). Reads now never write; every one of those transitions is owned by
//! `reconcile_status`, driven by `agent-task reconcile <run> --apply`, the daemon
//! tick, the runner terminal observer, and the mutating commands.

use super::*;
use crate::agent_task_lifecycle::{
    durable_state_for_test, seed_status_purity_fixtures_for_test, AgentTaskLifecycleStore,
};

/// Every library read `homeboy agent-task status` composes: the canonical
/// resource (alias-resolving and `--exact`), the durable record, and the
/// promotion-replay guidance it attaches.
fn status_reads(run_id: &str) {
    let _ = crate::orchestration::run_from_current_environment(run_id);
    let _ = crate::orchestration::run_exact_from_current_environment(run_id);
    let _ = crate::agent_task_service::control_plane_run(run_id);
    let _ = status(run_id);
    let _ = crate::agent_task_service::promotion_replay_guidance(run_id);
}

#[test]
fn status_leaves_every_lifecycle_state_byte_identical() {
    with_isolated_home(|_| {
        let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        let fixtures = seed_status_purity_fixtures_for_test().expect("seed fixtures");
        assert!(fixtures.len() >= 11, "{fixtures:?}");

        for fixture in &fixtures {
            let before = durable_state_for_test(&store);
            let record_before = store.read_record(&fixture.run_id).expect("fixture record");
            for _ in 0..2 {
                status_reads(&fixture.run_id);
                // Cook attempts are also read through their logical Cook id.
                if let Some(cook_id) = fixture.run_id.strip_suffix("-attempt") {
                    status_reads(cook_id);
                }
            }
            let record_after = store.read_record(&fixture.run_id).expect("fixture record");
            assert_eq!(
                record_after.revision, record_before.revision,
                "{}: status moved the store-owned revision",
                fixture.label
            );
            assert_eq!(
                durable_state_for_test(&store),
                before,
                "{}: status changed durable state (record, events, work intents, or files)",
                fixture.label
            );
        }
    });
}

/// The pending fixtures above are not inert: the explicit owner converges each
/// of them. This is what makes the purity assertion meaningful, and it is the
/// replacement for every test that used to observe these transitions through a
/// status read.
#[test]
fn reconcile_apply_owns_the_transitions_status_no_longer_performs() {
    with_isolated_home(|_| {
        let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        seed_status_purity_fixtures_for_test().expect("seed fixtures");

        // Persisted terminal runner observation -> terminal aggregate projection.
        let runner = "purity-pending-runner-snapshot";
        assert_eq!(
            store.read_record(runner).unwrap().state,
            AgentTaskRunState::Running
        );
        crate::agent_task_service::reconcile_run(runner, false).expect("reconcile runner run");
        let projected = store.read_record(runner).unwrap();
        assert_eq!(projected.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            projected.metadata["terminal_transport_recovery"],
            "persisted_runner_job_events"
        );
        assert!(store.read_aggregate(runner).is_ok());

        // Terminal Cook attempts -> durable continuation admission. A
        // candidate-recoverable Cook is exactly the #15668 shape: only an
        // explicit owner may enqueue its continuation, never a read.
        for run_id in [
            "purity-cook-succeeded-attempt",
            "purity-cook-recoverable-attempt",
        ] {
            assert!(
                store.read_record(run_id).unwrap().metadata["cook_continuation_scheduler"]
                    .is_null()
            );
            crate::agent_task_service::reconcile_run(run_id, false).expect("reconcile cook");
            assert_eq!(
                store.read_record(run_id).unwrap().metadata["cook_continuation_scheduler"]
                    ["status"],
                "queued",
                "{run_id}"
            );
        }

        // A running record whose owner process is gone -> cancelled.
        let orphan = "purity-running-dead-owner";
        let report = crate::agent_task_service::reconcile_run(orphan, false).expect("reconcile");
        assert_eq!(report.reconciled, 1, "{report:?}");
        assert_eq!(
            store.read_record(orphan).unwrap().state,
            AgentTaskRunState::Cancelled
        );
    });
}
