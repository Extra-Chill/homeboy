//! The lifecycle verdict, its mutation gate, and its wire shape.

use super::*;
use std::path::PathBuf;

fn attestation() -> Plan {
    Plan::NeedsAttestation(Attestation {
        state_dir: PathBuf::from("/daemon"),
        job_ids: vec![Uuid::nil()],
    })
}

#[test]
fn unleased_processes_refuse_mutation() {
    let planned = Plan::Blocked {
        cause: BlockCause::UnleasedProcess,
        reason: "pid 7 holds no lease".to_string(),
    };
    assert!(mutation_refusal(&planned).is_some(), "{planned:?}");
    let error = ensure_mutation_allowed_with("daemon ensure-running", || planned.clone())
        .expect_err("refused");
    assert_eq!(error.details["classification"], "lifecycle_refused");
    assert!(error
        .message
        .contains("daemon ensure-running refused by the daemon lifecycle plan"));
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
            "state_dir": "/daemon",
            "job_ids": [Uuid::nil()],
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
}
