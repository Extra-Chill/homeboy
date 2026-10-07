//! Apply lifecycle plans (#15557, slice C3).
//!
//! `daemon recover` asks [`plan_local`] what to do. Most transitions are still
//! carried out by the existing recovery executor. This module applies the
//! outcomes that executor has no step for, so a plan never dead-ends:
//!
//! - [`Step::StartDaemon`] and [`Step::ClaimAdmission`] when the legacy plan
//!   authorizes nothing;
//! - [`Plan::NeedsAttestation`]: the one operator confirmation, applied to
//!   the generation's complete job set.
//!
//! Every apply re-observes and re-plans first, and refuses unless the fresh
//! plan equals the one the operator reviewed. The precondition checked at
//! apply time is therefore the same predicate the planner used.

use std::path::Path;

use serde_json::json;

use super::lifecycle::observe_local;
use super::lifecycle_plan::{plan, BlockCause, Plan, Step, WaitCause};
use crate::error::{Error, Result};

/// Plan for this process's own daemon router.
pub fn plan_local() -> Plan {
    plan(&observe_local())
}

/// The shared safety gate for every mutating daemon entry point (#15557 C4).
///
/// Returns why `planned` forbids a lease or job-store mutation right now:
/// a dead generation still has a live workload process, or a live daemon
/// process holds no lease and could own the store being changed. Everything
/// else is left to the entry point's own, narrower preconditions.
pub fn mutation_refusal(planned: &Plan) -> Option<String> {
    match planned {
        Plan::Wait {
            cause: WaitCause::LiveWorkload,
            reason,
        } => Some(format!("live workload: {reason}")),
        Plan::Blocked {
            cause: BlockCause::UnleasedProcess,
            reason,
        } => Some(format!("unleased daemon process: {reason}")),
        _ => None,
    }
}

/// Refuse `entry_point` when the current lifecycle plan forbids mutation.
pub fn ensure_mutation_allowed(entry_point: &str) -> Result<()> {
    ensure_mutation_allowed_with(entry_point, plan_local)
}

fn ensure_mutation_allowed_with(entry_point: &str, replan: impl FnOnce() -> Plan) -> Result<()> {
    let Some(reason) = mutation_refusal(&replan()) else {
        return Ok(());
    };
    let mut error = Error::validation_invalid_argument(
        "daemon_lifecycle",
        format!("{entry_point} refused by the daemon lifecycle plan: {reason}"),
        None,
        Some(vec![
            "Run `homeboy daemon recover --dry-run` for the one next action.".to_string(),
        ]),
    );
    error.details["classification"] = json!("lifecycle_refused");
    Err(error)
}

/// Whether this module can apply `planned` (as opposed to the legacy
/// recovery executor).
pub fn applies(planned: &Plan) -> bool {
    matches!(
        planned,
        Plan::NeedsAttestation(_)
            | Plan::Transition {
                step: Step::StartDaemon | Step::ClaimAdmission { .. }
            }
    )
}

/// Apply `planned` after proving it is still the current plan.
///
/// Returns the applied step codes. `confirm_workload_processes_absent` is the
/// operator attestation an attestation plan requires; it is never assumed.
pub fn apply(
    planned: &Plan,
    confirm_workload_processes_absent: bool,
    addr: &str,
) -> Result<Vec<String>> {
    apply_with(
        planned,
        confirm_workload_processes_absent,
        plan_local,
        |planned| execute(planned, addr),
    )
}

fn apply_with(
    planned: &Plan,
    confirm_workload_processes_absent: bool,
    replan: impl FnOnce() -> Plan,
    execute: impl FnOnce(&Plan) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    if !applies(planned) {
        return Err(Error::validation_invalid_argument(
            "lifecycle_plan",
            format!("lifecycle plan {planned:?} is not applied by the lifecycle executor"),
            None,
            None,
        ));
    }
    if matches!(planned, Plan::NeedsAttestation(_)) && !confirm_workload_processes_absent {
        return Err(Error::validation_invalid_argument(
            "confirm_workload_processes_absent",
            "this recovery requires --confirm-workload-processes-absent after inspecting workload processes",
            None,
            None,
        ));
    }
    let current = replan();
    if &current != planned {
        let mut error = Error::validation_invalid_argument(
            "recovery_plan",
            format!(
                "daemon lifecycle changed after planning; no recovery steps were applied (planned {planned:?}, now {current:?})"
            ),
            Some("stale_daemon_recovery_plan".to_string()),
            Some(vec![
                "Re-run `homeboy daemon recover --dry-run` and review the current plan before applying.".to_string(),
            ]),
        );
        error.details["classification"] = json!("stale_daemon_recovery_plan");
        return Err(error);
    }
    execute(planned)
}

fn execute(planned: &Plan, addr: &str) -> Result<Vec<String>> {
    match planned {
        Plan::Transition {
            step: Step::StartDaemon,
        } => {
            super::ensure_running(addr)?;
            Ok(vec!["daemon.ensure_running".to_string()])
        }
        Plan::Transition {
            step:
                Step::ClaimAdmission {
                    from_lease_id,
                    to_lease_id,
                },
        } => {
            claim_admission(from_lease_id, to_lease_id)?;
            Ok(vec!["daemon.claim_admission".to_string()])
        }
        Plan::NeedsAttestation(attestation) => {
            let local_dir = crate::paths::daemon_state_file()?
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            if !super::same_directory(&attestation.state_dir, &local_dir) {
                // The executor's store is this process's state directory. A
                // sibling generation is reconciled in its own frame.
                return Err(Error::validation_invalid_argument(
                    "state_dir",
                    format!(
                        "the attestation targets generation {} in {}; run it in that directory: HOMEBOY_DAEMON_ROUTER_BYPASS=1 HOMEBOY_DAEMON_STATE_DIR={} homeboy daemon reconcile-dead-lease-orphans --lease-id {}{} --confirm-workload-processes-absent --no-replacement",
                        attestation.lease_id,
                        attestation.state_dir.display(),
                        attestation.state_dir.display(),
                        attestation.lease_id,
                        attestation
                            .job_ids
                            .iter()
                            .map(|id| format!(" --job-id {id}"))
                            .collect::<String>(),
                    ),
                    Some(attestation.state_dir.display().to_string()),
                    None,
                ));
            }
            super::reconcile_dead_lease_orphans(
                &attestation.lease_id,
                &attestation.job_ids,
                true,
                addr,
                true,
            )?;
            Ok(vec![
                super::recovery_actions::DAEMON_RECONCILE_DEAD_LEASE_ORPHANS.to_string(),
            ])
        }
        other => Err(Error::validation_invalid_argument(
            "lifecycle_plan",
            format!("lifecycle plan {other:?} has no lifecycle executor"),
            None,
            None,
        )),
    }
}

/// Move admission from a dead owner to the exact live generation `to`.
///
/// Re-proves both sides under the registry lock through the same predicate
/// daemon startup uses (#15456): `to` must hold a live lease in its own
/// directory, and `from` must be dead or replaced in place.
fn claim_admission(from_lease_id: &str, to_lease_id: &str) -> Result<()> {
    let view = observe_local();
    let target = view
        .generations
        .iter()
        .find(|generation| generation.lease_id.as_deref() == Some(to_lease_id))
        .ok_or_else(|| claim_refused(to_lease_id, "the target generation is no longer observed"))?;
    let validation = super::validate_lease_file(&target.state_dir.join("state.json"))?;
    let state = validation
        .state
        .filter(|state| validation.running && state.lease_id == to_lease_id)
        .ok_or_else(|| claim_refused(to_lease_id, "the target lease is not live"))?;
    let moved = super::generation_store::claim_admission_from_dead_owner(&state, |endpoint| {
        endpoint.lease_id == from_lease_id
            && super::admission_owner_is_dead(endpoint, Some(&target.state_dir))
    })?;
    if !moved {
        return Err(claim_refused(
            to_lease_id,
            "admission did not move: the recorded owner is not proven dead",
        ));
    }
    Ok(())
}

fn claim_refused(to_lease_id: &str, problem: &str) -> Error {
    Error::validation_invalid_argument(
        "lease_id",
        format!("refusing to move admission to {to_lease_id}: {problem}"),
        Some(to_lease_id.to_string()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::lifecycle_plan::{Attestation, CONFIRM_WORKLOAD_PROCESSES_ABSENT};
    use std::cell::Cell;
    use std::path::PathBuf;

    fn attestation() -> Plan {
        Plan::NeedsAttestation(Attestation {
            lease_id: "DEAD".to_string(),
            state_dir: PathBuf::from("/daemon"),
            job_ids: vec![uuid::Uuid::nil()],
            confirmation: CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string(),
        })
    }

    #[test]
    fn a_plan_that_changed_since_review_is_refused_without_executing() {
        let ran = Cell::new(false);
        let error = apply_with(
            &Plan::Transition {
                step: Step::StartDaemon,
            },
            false,
            || Plan::Converged,
            |_| {
                ran.set(true);
                Ok(Vec::new())
            },
        )
        .expect_err("a stale plan is refused");
        assert_eq!(
            error.details["classification"],
            "stale_daemon_recovery_plan"
        );
        assert!(!ran.get(), "nothing executes on a stale plan");
    }

    #[test]
    fn an_unchanged_plan_executes_once() {
        let planned = Plan::Transition {
            step: Step::ClaimAdmission {
                from_lease_id: "DEAD".to_string(),
                to_lease_id: "LIVE".to_string(),
            },
        };
        let runs = Cell::new(0);
        let applied = apply_with(
            &planned,
            false,
            || planned.clone(),
            |_| {
                runs.set(runs.get() + 1);
                Ok(vec!["daemon.claim_admission".to_string()])
            },
        )
        .expect("applied");
        assert_eq!(applied, vec!["daemon.claim_admission".to_string()]);
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn an_attestation_is_never_assumed() {
        let ran = Cell::new(false);
        let error = apply_with(&attestation(), false, attestation, |_| {
            ran.set(true);
            Ok(Vec::new())
        })
        .expect_err("attestation required");
        assert!(error
            .message
            .contains("--confirm-workload-processes-absent"));
        assert!(!ran.get());
        assert!(apply_with(&attestation(), true, attestation, |_| Ok(Vec::new())).is_ok());
    }

    #[test]
    fn the_mutation_gate_refuses_only_live_workload_and_unleased_processes() {
        let refused = [
            Plan::Wait {
                cause: WaitCause::LiveWorkload,
                reason: "child 9 is live".to_string(),
            },
            Plan::Blocked {
                cause: BlockCause::UnleasedProcess,
                reason: "pid 7 holds no lease".to_string(),
            },
        ];
        for planned in refused {
            let error = ensure_mutation_allowed_with("daemon adopt-orphan", || planned.clone())
                .expect_err("refused");
            assert_eq!(error.details["classification"], "lifecycle_refused");
            assert!(error
                .message
                .contains("daemon adopt-orphan refused by the daemon lifecycle plan"));
        }
        let allowed = [
            Plan::Converged,
            Plan::Wait {
                cause: WaitCause::BusyStaleDaemon,
                reason: "busy".to_string(),
            },
            Plan::Blocked {
                cause: BlockCause::Unreadable,
                reason: "legacy lease".to_string(),
            },
            Plan::Blocked {
                cause: BlockCause::UnknownSupervision,
                reason: "unknown".to_string(),
            },
            attestation(),
            Plan::Transition {
                step: Step::StartDaemon,
            },
        ];
        for planned in allowed {
            assert!(
                ensure_mutation_allowed_with("entry", || planned.clone()).is_ok(),
                "{planned:?}"
            );
        }
    }

    #[test]
    fn plans_the_legacy_executor_owns_are_not_applied_here() {
        for planned in [
            Plan::Converged,
            Plan::Wait {
                cause: crate::daemon::lifecycle_plan::WaitCause::LiveWorkload,
                reason: "live child".to_string(),
            },
            Plan::Blocked {
                cause: crate::daemon::lifecycle_plan::BlockCause::Unreadable,
                reason: "unreadable".to_string(),
            },
            Plan::Transition {
                step: Step::StopByLease {
                    lease_id: "OLD".to_string(),
                },
            },
        ] {
            assert!(!applies(&planned));
            assert!(apply_with(&planned, true, || planned.clone(), |_| Ok(Vec::new())).is_err());
        }
    }
}
