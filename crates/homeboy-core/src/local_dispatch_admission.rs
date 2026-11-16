//! Pressure-driven admission for locally dispatched Cook provider attempts.
//!
//! Homeboy already captures a resource-policy snapshot at preflight
//! ([`crate::resource_policy_context`]) and consults it for pressure-driven
//! Lab promotion (`lab_routing::authorizes_policy_lab_runner`). Local
//! dispatch never acted on it: an operator running several
//! `homeboy agent-task cook` invocations at once saw a resource-policy
//! warning naming the exact load ("machine is hot; ... Load average is 30.4
//! across 18 CPU(s)"), and Homeboy dispatched into it anyway — saturating the
//! machine and then failing the last providers on `provider_timeout`.
//!
//! This module is the missing enforcement. A counting-semaphore coordination
//! lease in the observation store (namespace [`NAMESPACE`], #15947) tracks how
//! many local Cook provider dispatches are active on this machine right now, and a pure decision function ([`evaluate_local_dispatch_admission`])
//! admits while pressure is ok or warm and queues while it is hot. Active
//! dispatch counts are tracked for observability, not used as a fixed ceiling.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::observation::coordination::{
    LeaseAcquire, LeaseAdmission, LeaseHolder, LeaseOwner, LeaseRequest,
};
use crate::observation::ObservationStore;
use crate::Result;

/// Coordination namespace owned by local Cook dispatch admission (#15947).
pub const NAMESPACE: &str = "local-cook-dispatch";

/// Every local dispatch on this machine competes for the same semaphore key.
const LEASE_KEY: &str = "local";

/// Upper bound on how long a slot survives without its holder releasing it.
/// Dead holders are reclaimed far sooner by process-identity reaping; the TTL
/// only bounds holders whose liveness cannot be proven either way.
const LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// RAII guard: releases the coordination lease when the local dispatch exits,
/// normally or on error. A crashed holder is reclaimed by the next caller's
/// [`ObservationStore::reap_coordination_leases`], which proves the owner gone
/// by process start identity, so a lease never permanently consumes a slot and
/// a reused PID never looks like a live owner.
#[derive(Debug)]
pub struct ActiveLocalDispatchLease {
    db_path: PathBuf,
    holder_token: String,
}

impl Drop for ActiveLocalDispatchLease {
    fn drop(&mut self) {
        if let Ok(store) = ObservationStore::open_initialized_for_lifecycle_at(&self.db_path) {
            let _ = store.release_coordination_lease(NAMESPACE, LEASE_KEY, &self.holder_token);
        }
    }
}

/// Outcome of [`try_admit_local_dispatch`].
#[derive(Debug)]
pub enum LocalDispatchAttempt {
    /// The decision admitted the dispatch and the slot is now held.
    Admitted(ActiveLocalDispatchLease),
    /// The decision queued the dispatch; no slot was taken.
    NotAdmitted(LocalDispatchAdmission),
}

/// Decision produced by [`evaluate_local_dispatch_admission`].
#[derive(Debug, Clone, PartialEq)]
pub enum LocalDispatchAdmission {
    /// Pressure is not hot: dispatch now, regardless of active count.
    Admit,
    /// Pressure is hot: wait for it to drop without taking a lease.
    Queue { reason: String },
}

/// Decide whether another local Cook provider dispatch may start.
///
/// `severity` is the already-captured resource-policy severity (`"ok"`,
/// `"warm"`, or `"hot"`); any other value is treated as `"ok"` so an absent or
/// unrecognized snapshot fails open rather than blocking dispatch on a host
/// this decision cannot evaluate.
///
/// Placement overrides authorize local execution but do not bypass waiting
/// for hot pressure to drop. Counts and overrides are retained as inputs for
/// callers sharing the resource-policy context; neither limits admission.
pub fn evaluate_local_dispatch_admission(
    _active_count: usize,
    severity: &str,
    load_one: Option<f64>,
    cpu_count: usize,
    _local_override: bool,
) -> LocalDispatchAdmission {
    if severity == "hot" {
        let load = load_one
            .map(|load| format!("load average {load:.1} across {cpu_count} CPU(s)"))
            .unwrap_or_else(|| format!("elevated load across {cpu_count} CPU(s)"));
        return LocalDispatchAdmission::Queue {
            reason: format!(
                "machine is hot ({load}); waiting for pressure to drop before local Cook dispatch"
            ),
        };
    }
    LocalDispatchAdmission::Admit
}

fn open_store(data_root: &Path) -> Result<ObservationStore> {
    ObservationStore::open_initialized_for_lifecycle_at(crate::paths::observation_db_in_root(
        data_root,
    ))
}

/// Slots a holder consumes. Every local dispatch weighs one slot.
fn holder_slots(holder: &LeaseHolder) -> usize {
    holder
        .weight
        .get("slots")
        .and_then(Value::as_u64)
        .map_or(1, |slots| slots as usize)
}

fn active_slots(holders: &[LeaseHolder]) -> usize {
    holders.iter().map(holder_slots).sum()
}

fn acquire_in(
    store: &ObservationStore,
    run_id: &str,
    admission: LeaseAdmission<'_>,
) -> Result<LeaseAcquire> {
    store.reap_coordination_leases(NAMESPACE)?;
    let weight = json!({ "slots": 1 });
    let payload = json!({
        "run_id": run_id,
        "started_at": chrono::Utc::now().to_rfc3339(),
    });
    store.acquire_coordination_lease(LeaseRequest {
        namespace: NAMESPACE,
        key: LEASE_KEY,
        ttl: LEASE_TTL,
        owner: LeaseOwner::current_process(),
        admission,
        weight: &weight,
        payload: &payload,
    })
}

fn guard(data_root: &Path, holder: LeaseHolder) -> ActiveLocalDispatchLease {
    ActiveLocalDispatchLease {
        db_path: crate::paths::observation_db_in_root(data_root),
        holder_token: holder.holder_token,
    }
}

/// Slots held by live local dispatches in the observation store below
/// `data_root`. Holders whose owner is provably gone (dead, or a reused PID
/// with a different start identity) are reaped first, so a dead holder never
/// permanently consumes a concurrency slot.
pub fn active_local_dispatch_count(data_root: &Path) -> Result<usize> {
    let store = open_store(data_root)?;
    store.reap_coordination_leases(NAMESPACE)?;
    Ok(active_slots(
        &store.coordination_lease_holders(NAMESPACE, LEASE_KEY)?,
    ))
}

/// Decide admission and take a slot in one store transaction.
///
/// `decide` receives the slots held by live dispatches and returns the
/// admission decision (normally [`evaluate_local_dispatch_admission`]). The
/// count and the acquire are judged inside the same IMMEDIATE transaction, so
/// counting remains consistent across concurrent callers.
pub fn try_admit_local_dispatch(
    data_root: &Path,
    run_id: &str,
    decide: impl Fn(usize) -> LocalDispatchAdmission,
) -> Result<LocalDispatchAttempt> {
    let store = open_store(data_root)?;
    let decision = Cell::new(None);
    let admit = |holders: &[LeaseHolder]| {
        let outcome = decide(active_slots(holders));
        let admitted = outcome == LocalDispatchAdmission::Admit;
        decision.set(Some(outcome));
        admitted
    };
    match acquire_in(&store, run_id, LeaseAdmission::Capacity(&admit))? {
        LeaseAcquire::Acquired(holder) => {
            Ok(LocalDispatchAttempt::Admitted(guard(data_root, holder)))
        }
        LeaseAcquire::Contended(holders) => Ok(LocalDispatchAttempt::NotAdmitted(
            decision
                .take()
                .unwrap_or_else(|| decide(active_slots(&holders))),
        )),
    }
}

/// Unconditionally acquire a slot for `run_id` below `data_root`.
///
/// Callers that need admission use [`try_admit_local_dispatch`]; this only
/// records a slot (for example an already-running dispatch). The returned
/// guard releases the slot on drop.
pub fn acquire_local_dispatch_lease(
    data_root: &Path,
    run_id: &str,
) -> Result<ActiveLocalDispatchLease> {
    let store = open_store(data_root)?;
    let accept = |_: &[LeaseHolder]| true;
    match acquire_in(&store, run_id, LeaseAdmission::Capacity(&accept))? {
        LeaseAcquire::Acquired(holder) => Ok(guard(data_root, holder)),
        LeaseAcquire::Contended(_) => Err(crate::Error::internal_unexpected(
            "unconditional local dispatch lease was refused",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_depends_on_pressure_not_active_count() {
        for active in [0, 1, 8, 100, usize::MAX] {
            for local_override in [false, true] {
                for severity in ["ok", "warm", "unknown"] {
                    assert_eq!(
                        evaluate_local_dispatch_admission(
                            active,
                            severity,
                            None,
                            32,
                            local_override
                        ),
                        LocalDispatchAdmission::Admit
                    );
                }
                let admission = evaluate_local_dispatch_admission(
                    active,
                    "hot",
                    Some(30.4),
                    18,
                    local_override,
                );
                let LocalDispatchAdmission::Queue { reason } = admission else {
                    panic!("hot pressure must queue: {admission:?}");
                };
                assert!(reason.contains("hot"), "{reason}");
                assert!(reason.contains("30.4"), "{reason}");
                assert!(reason.contains("18 CPU"), "{reason}");
            }
        }
    }

    #[test]
    fn acquiring_and_dropping_a_lease_changes_the_active_count() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);

        let lease = acquire_local_dispatch_lease(data_root, "run-a").expect("acquire first lease");
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);

        let second =
            acquire_local_dispatch_lease(data_root, "run-b").expect("acquire second lease");
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 2);

        drop(lease);
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);

        drop(second);
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
    }

    /// Active leases do not limit admission while pressure is ok.
    #[test]
    fn multiple_claimants_are_admitted_without_a_fixed_ceiling() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let decide = |active| evaluate_local_dispatch_admission(active, "ok", None, 4, false);

        let first = try_admit_local_dispatch(data_root, "run-a", decide).expect("first attempt");
        let second = try_admit_local_dispatch(data_root, "run-b", decide).expect("second attempt");

        assert!(matches!(first, LocalDispatchAttempt::Admitted(_)));
        assert!(matches!(second, LocalDispatchAttempt::Admitted(_)));
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 2);
    }

    /// A queued decision takes no slot.
    #[test]
    fn a_queued_attempt_holds_no_slot() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let attempt = try_admit_local_dispatch(data_root, "run-hot", |active| {
            evaluate_local_dispatch_admission(active, "hot", Some(30.4), 18, false)
        })
        .expect("attempt");
        assert!(matches!(
            attempt,
            LocalDispatchAttempt::NotAdmitted(LocalDispatchAdmission::Queue { .. })
        ));
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
    }

    /// Leases live in the store, not the process: a reopened store (a fresh
    /// connection, as a second cook process would open) sees the held slot.
    #[test]
    fn a_held_slot_survives_reopening_the_store() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let lease = acquire_local_dispatch_lease(data_root, "run-a").expect("acquire");

        let reopened = open_store(data_root).expect("reopen store");
        let holders = reopened
            .coordination_lease_holders(NAMESPACE, LEASE_KEY)
            .expect("holders");
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].payload["run_id"], "run-a");

        drop(lease);
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
    }

    /// The PID-reuse bug this migration fixes: a holder recorded with this
    /// process's PID but a different process start identity is a different,
    /// dead process that happened to get the same PID. It must not hold a
    /// slot; the next count reaps it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_reused_pid_with_a_different_start_identity_does_not_hold_a_slot() {
        use crate::process::ProcessStartIdentity;

        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let live = LeaseOwner::current_process();
        let reused_pid = LeaseOwner {
            start_identity: Some(
                match live
                    .start_identity
                    .clone()
                    .expect("current process start identity")
                {
                    ProcessStartIdentity::Linux { starttime_ticks } => {
                        ProcessStartIdentity::Linux {
                            starttime_ticks: starttime_ticks.wrapping_add(1),
                        }
                    }
                    ProcessStartIdentity::Macos {
                        start_seconds,
                        start_microseconds,
                    } => ProcessStartIdentity::Macos {
                        start_seconds: start_seconds.wrapping_add(1),
                        start_microseconds,
                    },
                },
            ),
            ..live
        };
        let store = open_store(data_root).expect("open store");
        let weight = json!({ "slots": 1 });
        let payload = json!({ "run_id": "stale-run" });
        let accept = |_: &[LeaseHolder]| true;
        let outcome = store
            .acquire_coordination_lease(LeaseRequest {
                namespace: NAMESPACE,
                key: LEASE_KEY,
                ttl: LEASE_TTL,
                owner: reused_pid,
                admission: LeaseAdmission::Capacity(&accept),
                weight: &weight,
                payload: &payload,
            })
            .expect("seed reused-pid holder");
        assert!(matches!(outcome, LeaseAcquire::Acquired(_)));
        assert_eq!(
            store
                .coordination_lease_holders(NAMESPACE, LEASE_KEY)
                .unwrap()
                .len(),
            1,
            "the reused-pid holder row exists before reaping"
        );

        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);

        // A real dispatch is admitted after the stale holder is reaped.
        let attempt = try_admit_local_dispatch(data_root, "fresh-run", |active| {
            evaluate_local_dispatch_admission(active, "ok", None, 4, false)
        })
        .expect("attempt");
        assert!(matches!(attempt, LocalDispatchAttempt::Admitted(_)));
    }
}
