//! `homeboy agent-task status` is a pure projection (#13697, #15718 step 3).
//!
//! The fixtures cover every lifecycle shape a reconciling read used to act on:
//! queued, running with a dead owner, every terminal state, a Lab run with an
//! unprojected terminal runner snapshot, and terminal Cook attempts (including a
//! candidate-recoverable one, the #15668 shape) whose continuation is not yet
//! enqueued. `agent-task reconcile <run> --apply` is the explicit owner of
//! those transitions; status must not perform any of them.

use super::support::*;
use homeboy::agents::agent_task_lifecycle::{
    durable_state_for_test, seed_status_purity_fixtures_for_test, AgentTaskLifecycleStore,
};

fn status_args(run_id: &str, full: bool, exact: bool, strict_subject_exit: bool) -> StatusArgs {
    StatusArgs {
        run_id: run_id.to_string(),
        full,
        exact,
        strict_subject_exit,
        watch: false,
        interval: "5s".to_string(),
        timeout: "30m".to_string(),
    }
}

#[test]
fn agent_task_status_leaves_record_revision_events_and_work_intents_unchanged() {
    with_isolated_home(|_| {
        let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        let fixtures = seed_status_purity_fixtures_for_test().expect("seed fixtures");

        for fixture in &fixtures {
            let before = durable_state_for_test(&store);
            let revision = store
                .read_record(&fixture.run_id)
                .expect("fixture record")
                .revision;
            for (full, exact, strict) in [
                (false, false, false),
                (true, false, false),
                (false, true, false),
                (true, true, true),
            ] {
                status(status_args(&fixture.run_id, full, exact, strict)).unwrap_or_else(|error| {
                    panic!("{}: status failed: {}", fixture.label, error.message)
                });
                // Cook attempts are also read through their logical Cook id.
                if let Some(cook_id) = fixture.run_id.strip_suffix("-attempt") {
                    status(status_args(cook_id, full, false, strict)).unwrap_or_else(|error| {
                        panic!("{}: cook status failed: {}", fixture.label, error.message)
                    });
                }
            }
            assert_eq!(
                store.read_record(&fixture.run_id).unwrap().revision,
                revision,
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

        // The Cook fixtures were not inert: the explicit owner admits their
        // continuation, which status above did not.
        let (_, exit) = reconcile_run(ReconcileArgs {
            run_id: "purity-cook-recoverable-attempt".to_string(),
            dry_run: false,
            apply: true,
            idempotency_key: Some("status-purity".to_string()),
        })
        .expect("reconcile --apply");
        assert_eq!(exit, 0);
        assert_eq!(
            store
                .read_record("purity-cook-recoverable-attempt")
                .unwrap()
                .metadata["cook_continuation_scheduler"]["status"],
            "queued"
        );
    });
}
