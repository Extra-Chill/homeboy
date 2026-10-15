//! The lifecycle verdict, its mutation gate, and its wire shape.

use super::*;
use std::path::PathBuf;

fn attestation() -> Plan {
    Plan::NeedsAttestation(Attestation {
        lease_id: "DEAD".to_string(),
        state_dir: PathBuf::from("/daemon"),
        job_ids: vec![Uuid::nil()],
        confirmation: CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string(),
    })
}

#[test]
fn live_workload_and_unleased_processes_refuse_mutation() {
    for planned in [
        Plan::Wait {
            cause: WaitCause::LiveWorkload,
            reason: "child 9 is live".to_string(),
        },
        Plan::Blocked {
            cause: BlockCause::UnleasedProcess,
            reason: "pid 7 holds no lease".to_string(),
        },
    ] {
        assert!(mutation_refusal(&planned).is_some(), "{planned:?}");
        let error = ensure_mutation_allowed_with("daemon adopt-orphan", || planned.clone())
            .expect_err("refused");
        assert_eq!(error.details["classification"], "lifecycle_refused");
        assert!(error
            .message
            .contains("daemon adopt-orphan refused by the daemon lifecycle plan"));
    }
}

#[test]
fn other_verdicts_leave_mutation_to_the_entry_points_own_gates() {
    for planned in [
        Plan::Converged,
        attestation(),
        Plan::Wait {
            cause: WaitCause::BusyStaleDaemon,
            reason: "stale daemon owns jobs".to_string(),
        },
        Plan::Blocked {
            cause: BlockCause::Unreadable,
            reason: "corrupt lease".to_string(),
        },
        Plan::Blocked {
            cause: BlockCause::Unauthorized,
            reason: "no authorized recovery".to_string(),
        },
        Plan::Transition {
            step: Step::Repair {
                lease_id: Some("lease".to_string()),
                codes: vec!["daemon_ensure_running".to_string()],
            },
        },
    ] {
        assert_eq!(mutation_refusal(&planned), None, "{planned:?}");
        ensure_mutation_allowed_with("entry", || planned.clone()).expect("not refused");
    }
}

/// Remote callers parse `daemon plan` with this exact shape.
#[test]
fn plans_serialize_with_stable_tags() {
    assert_eq!(
        serde_json::to_value(attestation()).unwrap(),
        serde_json::json!({
            "plan": "needs_attestation",
            "lease_id": "DEAD",
            "state_dir": "/daemon",
            "job_ids": [Uuid::nil()],
            "confirmation": "confirm-workload-processes-absent",
        })
    );
    assert_eq!(
        serde_json::to_value(Plan::Transition {
            step: Step::Repair {
                lease_id: None,
                codes: vec!["daemon_ensure_running".to_string()],
            }
        })
        .unwrap(),
        serde_json::json!({
            "plan": "transition",
            "step": { "step": "repair", "codes": ["daemon_ensure_running"] }
        })
    );
    assert_eq!(
        serde_json::to_value(Plan::Converged).unwrap(),
        serde_json::json!({ "plan": "converged" })
    );
    let decoded: Plan =
        serde_json::from_value(serde_json::to_value(attestation()).unwrap()).unwrap();
    assert_eq!(decoded, attestation());
}

/// The local plan reads the one daemon store. With no daemon and no jobs it
/// never errors: an unreadable status is itself a verdict.
#[test]
fn the_local_plan_reads_the_one_store_and_never_errors() {
    crate::test_support::with_isolated_home(|_| {
        let status = crate::daemon::read_status().expect("status of an empty store");
        let _ = plan_local();
        assert_eq!(
            plan(&status),
            crate::daemon::recovery_actions::plan_recovery(&status).lifecycle
        );
    });
}
