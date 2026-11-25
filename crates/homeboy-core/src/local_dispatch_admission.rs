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
//! many local Cook provider dispatches are active on this machine right now,
//! and [`admit_local_dispatch`] admits while host load is ok or warm and
//! queues while it is hot. Active dispatch counts are tracked for
//! observability, not used as a fixed ceiling.
//!
//! The module also owns the host load reading ([`host_load_averages`]), its
//! classification ([`classify_load_severity`]) and the evidence a queued
//! dispatch records ([`local_dispatch_wait_evidence`]). The Cook engine is the
//! only caller of admission (#18922).

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
enum LocalDispatchAttempt {
    /// The decision admitted the dispatch and the slot is now held.
    Admitted(ActiveLocalDispatchLease),
    /// The decision queued the dispatch; no slot was taken.
    NotAdmitted(LocalDispatchAdmission),
}

/// Decision produced by [`evaluate_local_dispatch_admission`].
#[derive(Debug, Clone, PartialEq)]
enum LocalDispatchAdmission {
    /// Pressure is not hot: dispatch now, regardless of active count.
    Admit,
    /// Pressure is hot: wait for it to drop without taking a lease.
    Queue { reason: String },
}

/// How often a queued local dispatch re-samples host pressure.
pub const LOCAL_DISPATCH_QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// 1-minute load per CPU at or above which the host is hot.
const HOT_LOAD_ONE_PER_CPU: f64 = 1.5;
/// 5-minute load per CPU at or above which the host is hot.
const HOT_LOAD_FIVE_PER_CPU: f64 = 1.25;
/// Load per CPU (either window) at or above which the host is warm.
const WARM_LOAD_PER_CPU: f64 = 0.75;

/// Test-build override for the host load reading, as `one,five,fifteen`.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_LOAD_AVERAGES_ENV: &str = "HOMEBOY_TEST_LOAD_AVERAGES";

#[cfg(any(test, feature = "test-support"))]
fn test_load_averages() -> Option<[f64; 3]> {
    let raw = std::env::var(TEST_LOAD_AVERAGES_ENV).ok()?;
    let mut values = raw.split(',').map(str::parse::<f64>);
    Some([
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ])
}

/// The host's 1/5/15-minute load averages, when the platform reports them.
/// Test builds read [`TEST_LOAD_AVERAGES_ENV`] first.
pub fn host_load_averages() -> Option<[f64; 3]> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(values) = test_load_averages() {
        return Some(values);
    }
    homeboy_engine_primitives::host_telemetry::load_average()
        .map(|load| [load.one, load.five, load.fifteen])
}

/// Logical CPUs available to this process.
pub fn host_cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

/// Classify CPU-normalized load as `"ok"`, `"warm"`, or `"hot"`. Unknown load
/// is `"ok"`, so a host this cannot measure is never blocked.
pub fn classify_load_severity(averages: Option<[f64; 3]>, cpu_count: usize) -> &'static str {
    let Some([one, five, _]) = averages else {
        return "ok";
    };
    let cpus = cpu_count.max(1) as f64;
    let (one_ratio, five_ratio) = (one / cpus, five / cpus);
    if one_ratio >= HOT_LOAD_ONE_PER_CPU || five_ratio >= HOT_LOAD_FIVE_PER_CPU {
        "hot"
    } else if one_ratio >= WARM_LOAD_PER_CPU || five_ratio >= WARM_LOAD_PER_CPU {
        "warm"
    } else {
        "ok"
    }
}

/// One host pressure reading that local dispatch admission judges.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalDispatchPressure {
    /// `"ok"`, `"warm"`, or `"hot"`.
    pub severity: &'static str,
    pub load_one: Option<f64>,
    pub load_five: Option<f64>,
    pub cpu_count: usize,
}

impl LocalDispatchPressure {
    pub fn from_load(averages: Option<[f64; 3]>, cpu_count: usize) -> Self {
        Self {
            severity: classify_load_severity(averages, cpu_count),
            load_one: averages.map(|values| values[0]),
            load_five: averages.map(|values| values[1]),
            cpu_count,
        }
    }
}

/// Sample host pressure for local dispatch admission.
///
/// Test builds only measure when [`TEST_LOAD_AVERAGES_ENV`] is set: parallel
/// test processes load the host themselves, and a real reading would queue
/// fixture Cooks indefinitely. `None` means admission is not evaluated.
pub fn sample_local_dispatch_pressure() -> Option<LocalDispatchPressure> {
    #[cfg(any(test, feature = "test-support"))]
    if std::env::var_os(TEST_LOAD_AVERAGES_ENV).is_none() {
        return None;
    }
    Some(LocalDispatchPressure::from_load(
        host_load_averages(),
        host_cpu_count(),
    ))
}

/// Decide whether another local Cook provider dispatch may start: admit while
/// pressure is ok or warm, queue while it is hot. The active count is not a
/// ceiling.
fn evaluate_local_dispatch_admission(
    _active_count: usize,
    pressure: &LocalDispatchPressure,
) -> LocalDispatchAdmission {
    if pressure.severity == "hot" {
        let cpu_count = pressure.cpu_count;
        let load = pressure
            .load_one
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

/// Live Homeboy runs holding local dispatch slots.
pub fn local_dispatch_holders(data_root: &Path) -> Result<Vec<Value>> {
    let store = open_store(data_root)?;
    store.reap_coordination_leases(NAMESPACE)?;
    Ok(store
        .coordination_lease_holders(NAMESPACE, LEASE_KEY)?
        .into_iter()
        .map(|holder| holder.payload)
        .collect())
}

/// Decide admission and take a slot in one store transaction.
///
/// `decide` receives the slots held by live dispatches and returns the
/// admission decision. The count and the acquire are judged inside the same
/// IMMEDIATE transaction, so counting remains consistent across concurrent
/// callers.
fn try_admit_local_dispatch(
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

/// Admit a local dispatch for `run_id`, waiting while pressure is hot.
///
/// `pressure` is re-sampled on every poll: a hot reading never cools on its
/// own. Each refusal calls `on_queue` with the reason and the reading, then
/// sleeps `poll_interval`. There is no admission timeout; the caller owns its
/// deadline. The returned guard holds the slot until dropped.
pub fn admit_local_dispatch(
    data_root: &Path,
    run_id: &str,
    mut pressure: impl FnMut() -> Result<LocalDispatchPressure>,
    poll_interval: Duration,
    mut sleep: impl FnMut(Duration),
    mut on_queue: impl FnMut(&str, &LocalDispatchPressure),
) -> Result<ActiveLocalDispatchLease> {
    loop {
        let reading = pressure()?;
        let attempt = try_admit_local_dispatch(data_root, run_id, |active| {
            evaluate_local_dispatch_admission(active, &reading)
        })?;
        match attempt {
            LocalDispatchAttempt::Admitted(lease) => return Ok(lease),
            // An admitting decision always takes the slot in the same
            // transaction; re-run admission rather than trust a stale one.
            LocalDispatchAttempt::NotAdmitted(LocalDispatchAdmission::Admit) => continue,
            LocalDispatchAttempt::NotAdmitted(LocalDispatchAdmission::Queue { reason }) => {
                on_queue(&reason, &reading);
                sleep(poll_interval);
            }
        }
    }
}

/// Evidence a queued dispatch records for `status` and `diagnose`: the
/// reading, its thresholds, the live slot holders and the heaviest process
/// groups. Sampled only when admission refuses, not on every status read.
pub fn local_dispatch_wait_evidence(
    data_root: &Path,
    pressure: &LocalDispatchPressure,
    poll_interval: Duration,
) -> Value {
    let cpus = pressure.cpu_count.max(1) as f64;
    let mut holders = local_dispatch_holders(data_root).unwrap_or_default();
    let processes = std::process::Command::new("ps")
        .args(["-axo", "pid=,user=,pcpu=,rss=,comm="])
        .output();
    let (groups, probe_error) = match processes {
        Ok(output) if output.status.success() => (
            group_pressure_processes(&String::from_utf8_lossy(&output.stdout)),
            None,
        ),
        Ok(output) => (Vec::new(), Some(format!("ps exited {}", output.status))),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    holders.extend(groups);
    let retry_after =
        chrono::Duration::from_std(poll_interval).unwrap_or_else(|_| chrono::Duration::zero());
    json!({
        "local_dispatch_wait": {
            "pressure": {"load1": pressure.load_one, "load5": pressure.load_five,
                "recommendation": pressure.severity},
            "pressure_thresholds": {"load1": cpus * HOT_LOAD_ONE_PER_CPU,
                "load5": cpus * HOT_LOAD_FIVE_PER_CPU},
            "pressure_holders": holders,
            "process_probe_error": probe_error,
            "next_attempt_at": (chrono::Utc::now() + retry_after).to_rfc3339(),
            "queue_position": null,
            "eta_seconds": null,
            "queue_policy": "pressure polling; no ordered queue or admission ETA"
        }
    })
}

/// Group by executable and OS user, retaining PIDs and aggregate CPU/RSS.
/// `comm` deliberately avoids arguments (which may contain credentials).
fn group_pressure_processes(output: &str) -> Vec<Value> {
    let mut groups = std::collections::BTreeMap::<(String, String), (Vec<u32>, f64, u64)>::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(user), Some(cpu), Some(rss)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(cpu), Ok(rss)) =
            (pid.parse::<u32>(), cpu.parse::<f64>(), rss.parse::<u64>())
        else {
            continue;
        };
        let command = fields.collect::<Vec<_>>().join(" ");
        if command.is_empty() {
            continue;
        }
        let group = groups.entry((user.to_string(), command)).or_default();
        group.0.push(pid);
        group.1 += cpu;
        group.2 = group.2.saturating_add(rss);
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1 .1.total_cmp(&a.1 .1).then_with(|| b.1 .2.cmp(&a.1 .2)));
    // Include memory-heavy groups even when they are currently sleeping.
    let mut memory = groups.clone();
    memory.sort_by_key(|entry| std::cmp::Reverse(entry.1 .2));
    let selected: std::collections::BTreeSet<_> = groups
        .iter()
        .take(8)
        .chain(memory.iter().take(8))
        .map(|entry| entry.0.clone())
        .collect();
    groups
        .into_iter()
        .filter(|entry| selected.contains(&entry.0))
        .map(|((user, command), (pids, cpu, rss))| {
            json!({
                "kind": "process_group", "user": user, "command": command,
                "count": pids.len(), "pids": pids, "cpu_percent": cpu, "rss_kb": rss
            })
        })
        .collect()
}

/// Unconditionally acquire a slot for `run_id` below `data_root`.
///
/// Callers that need admission use [`admit_local_dispatch`]; this only
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

    fn reading(
        severity: &'static str,
        load_one: Option<f64>,
        cpu_count: usize,
    ) -> LocalDispatchPressure {
        LocalDispatchPressure {
            severity,
            load_one,
            load_five: load_one,
            cpu_count,
        }
    }

    #[test]
    fn admission_depends_on_pressure_not_active_count() {
        for active in [0, 1, 8, 100, usize::MAX] {
            for severity in ["ok", "warm"] {
                assert_eq!(
                    evaluate_local_dispatch_admission(active, &reading(severity, None, 32)),
                    LocalDispatchAdmission::Admit
                );
            }
            let admission =
                evaluate_local_dispatch_admission(active, &reading("hot", Some(30.4), 18));
            let LocalDispatchAdmission::Queue { reason } = admission else {
                panic!("hot pressure must queue: {admission:?}");
            };
            assert!(reason.contains("hot"), "{reason}");
            assert!(reason.contains("30.4"), "{reason}");
            assert!(reason.contains("18 CPU"), "{reason}");
        }
    }

    #[test]
    fn classifies_load_by_cpu_normalized_pressure() {
        assert_eq!(classify_load_severity(None, 4), "ok");
        assert_eq!(classify_load_severity(Some([1.0, 1.0, 1.0]), 4), "ok");
        assert_eq!(classify_load_severity(Some([3.1, 2.0, 1.0]), 4), "warm");
        assert_eq!(classify_load_severity(Some([6.0, 4.0, 2.0]), 4), "hot");
        assert_eq!(classify_load_severity(Some([1.0, 5.0, 1.0]), 4), "hot");
    }

    /// Warm pressure admits beyond any former CPU ceiling without waiting.
    #[test]
    fn warm_pressure_admits_without_waiting() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let _holders: Vec<_> = (0..4)
            .map(|index| {
                acquire_local_dispatch_lease(data_root, &format!("holder-{index}"))
                    .expect("acquire holder lease")
            })
            .collect();

        let lease = admit_local_dispatch(
            data_root,
            "queued-run",
            || Ok(reading("warm", Some(10.0), 18)),
            Duration::from_secs(1),
            |_| panic!("warm pressure must not wait"),
            |_, _| panic!("warm pressure must not queue"),
        );

        assert!(lease.is_ok(), "{:?}", lease.err());
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 5);
    }

    /// Hot pressure queues without a slot, past the old 1800s timeout, and
    /// admits once a refreshed reading drops.
    #[test]
    fn hot_pressure_queues_without_a_slot_until_pressure_drops() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let elapsed = Cell::new(Duration::ZERO);
        let mut reasons = Vec::new();

        let lease = admit_local_dispatch(
            data_root,
            "queued-run",
            || {
                Ok(if elapsed.get().as_secs() > 1830 {
                    reading("ok", Some(1.0), 18)
                } else {
                    reading("hot", Some(30.4), 18)
                })
            },
            Duration::from_secs(15),
            |interval| {
                assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
                elapsed.set(elapsed.get() + interval);
            },
            |reason, pressure| {
                assert_eq!(pressure.severity, "hot");
                reasons.push(reason.to_string());
            },
        );

        assert!(lease.is_ok(), "{lease:?}");
        assert!(
            reasons.len() > 120,
            "must keep queueing past the old timeout"
        );
        assert!(reasons[0].contains("30.4") && reasons[0].contains("18 CPU"));
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);
        drop(lease);
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
    }

    #[test]
    fn synthetic_php_servers_are_grouped_by_user_with_pids_and_usage() {
        let groups = group_pressure_processes(
            "101 chubes 50.0 1024 php\n102 chubes 25.0 2048 php\n103 other 1.0 512 php\nmalformed\n",
        );
        assert_eq!(groups[0]["command"], "php");
        assert_eq!(groups[0]["user"], "chubes");
        assert_eq!(groups[0]["count"], 2);
        assert_eq!(groups[0]["pids"], json!([101, 102]));
        assert_eq!(groups[0]["cpu_percent"], 75.0);
        assert_eq!(groups[0]["rss_kb"], 3072);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn wait_evidence_names_holders_thresholds_and_queue_policy() {
        let temp = tempfile::tempdir().expect("temp dir");
        let _lease = acquire_local_dispatch_lease(temp.path(), "holder-run").expect("acquire");
        let evidence = local_dispatch_wait_evidence(
            temp.path(),
            &reading("hot", Some(30.4), 18),
            LOCAL_DISPATCH_QUEUE_POLL_INTERVAL,
        );
        let wait = &evidence["local_dispatch_wait"];
        assert_eq!(wait["pressure"]["recommendation"], "hot");
        assert_eq!(wait["pressure_holders"][0]["run_id"], "holder-run");
        assert_eq!(wait["pressure_thresholds"]["load1"], 27.0);
        assert!(
            chrono::DateTime::parse_from_rfc3339(wait["next_attempt_at"].as_str().unwrap()).is_ok()
        );
        assert!(wait["queue_policy"]
            .as_str()
            .unwrap()
            .contains("no ordered queue"));
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

    /// A holder recorded with this process's PID but a different process start
    /// identity is a dead process whose PID was reused. It must not hold a
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
        let ok = reading("ok", None, 4);
        let attempt = try_admit_local_dispatch(data_root, "fresh-run", |active| {
            evaluate_local_dispatch_admission(active, &ok)
        })
        .expect("attempt");
        assert!(matches!(attempt, LocalDispatchAttempt::Admitted(_)));
    }
}
