//! These tests also run against the original production boundary to reproduce
//! the terminal-winner spin with test-owned cleanup rather than a leaked worker.

use super::*;
use crate::api_jobs::{ControllerJobState, ControllerJobSubmissionOutcome};

pub(super) fn running_controller(store: &JobStore, job_type: &str) -> Uuid {
    let submitted = store
        .admit_controller_job(
            format!("controller.{job_type}"),
            Uuid::new_v4().to_string(),
            ControllerJobState {
                job_type: job_type.to_string(),
                version: 1,
                request: json!({}),
                public_request: json!({}),
                request_digest: "regression".to_string(),
                active_idempotency_key: None,
                linked_durable_run_id: None,
                checkpoint: Some(json!({"phase": "prepared"})),
                cancellation_requested: false,
                cancellation_reason: None,
                execution_claim_id: None,
                recovery_attempted: false,
            },
        )
        .unwrap();
    let ControllerJobSubmissionOutcome::Submitted(id) = submitted else {
        panic!("unique fixture")
    };
    store.claim_controller_execution(id, false).unwrap();
    id
}

#[test]
fn controller_terminal_winner_stops_stale_success_worker_with_owned_deadline() {
    for status in [
        JobStatus::Succeeded,
        JobStatus::Failed,
        JobStatus::Cancelled,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path).unwrap();
        let id = running_controller(&store, "test.terminal-winner");
        let winner = JobStore::open_without_reconciliation(&path).unwrap();
        match status {
            JobStatus::Succeeded => {
                winner
                    .complete_controller_success(id, json!({"winner": true}))
                    .unwrap();
            }
            JobStatus::Failed => {
                winner
                    .fail_controller_error(id, "winner failed".to_string(), json!({"winner": true}))
                    .unwrap();
            }
            JobStatus::Cancelled => {
                winner
                    .request_controller_cancellation(id, "winner cancelled".to_string())
                    .unwrap();
                winner.complete_controller_cancellation(id).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fs::read(&path).unwrap();
        let handle = store.handle(id);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(persist_controller_success_or_uncertainty(
                    &handle,
                    json!({"losing_result": true}),
                ))
                .unwrap();
        });
        let observed = receiver.recv_timeout(Duration::from_secs(2));
        if observed.is_err() {
            // Original code spins on a succeeded/failed winner. Removing this
            // exact terminal fixture makes its cancellation check observe
            // absence, so even the red reproduction joins its worker.
            store
                .prune_terminal_controller_jobs("test.terminal-winner", 1, &[id])
                .unwrap();
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("fixture cleanup stops original worker");
        }
        worker.join().unwrap();
        assert_eq!(
            observed.unwrap(),
            true,
            "a terminal winner ends persistence without a second cancellation"
        );
        assert_eq!(store.get(id).unwrap().status, status);
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "loser cannot overwrite evidence or append events"
        );
    }
}

#[test]
fn controller_terminal_duplicate_observes_other_store_without_rewriting_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).unwrap();
    let id = running_controller(&store, "test.terminal-duplicate");
    let winner = JobStore::open_without_reconciliation(&path).unwrap();
    winner
        .complete_controller_success(id, json!({"original": "x".repeat(64 * 1024)}))
        .unwrap();
    let before = fs::read(&path).unwrap();
    for _ in 0..64 {
        assert_eq!(
            store
                .complete_controller_success(id, json!({"duplicate": true}))
                .unwrap()
                .status,
            JobStatus::Succeeded
        );
        assert_eq!(
            store
                .fail_controller_error(id, "late failure".to_string(), json!({}))
                .unwrap()
                .status,
            JobStatus::Succeeded
        );
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(store.events(id).unwrap().len(), 3);
}
