//! Concurrency ceiling for locally dispatched Cook provider attempts (#14732).
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
//! This module is the missing enforcement. A lightweight, cross-process lease
//! directory tracks how many local Cook provider dispatches are active on this
//! machine right now, and a pure decision function ([`evaluate_local_dispatch_admission`])
//! turns that count plus the already-captured pressure severity into one of
//! three outcomes: admit, queue, or refuse. Refusal names the observed load so
//! the operator sees the same evidence the resource-policy warning already
//! computed, instead of a second silent saturation.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// On-disk lease held by one active local Cook provider dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalDispatchLease {
    run_id: String,
    pid: u32,
    started_at: String,
}

/// RAII guard: removes the lease when the local dispatch exits, normally or on
/// error. A crashed holder is reclaimed by the next caller's liveness prune in
/// [`active_local_dispatch_count`], so a lease never permanently consumes a
/// slot.
#[derive(Debug)]
pub struct ActiveLocalDispatchLease {
    path: PathBuf,
    pid: u32,
}

impl Drop for ActiveLocalDispatchLease {
    fn drop(&mut self) {
        if let Ok(Some(lease)) = read_lease(&self.path) {
            if lease.pid == self.pid {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// Decision produced by [`evaluate_local_dispatch_admission`].
#[derive(Debug, Clone, PartialEq)]
pub enum LocalDispatchAdmission {
    /// Fewer local dispatches are active than the ceiling: dispatch now.
    Admit,
    /// The ceiling is already reached but pressure has not crossed the
    /// refusal threshold: queue (wait for a slot) rather than dispatch
    /// immediately.
    Queue { active: usize, ceiling: usize },
    /// Pressure severity already exceeds the refusal threshold: do not
    /// dispatch or queue. `reason` names the observed load.
    Refuse { reason: String },
}

/// How many local Cook provider dispatches this host admits concurrently.
///
/// A local provider attempt regularly bursts past one core (compiling,
/// running a test suite, a second agent subprocess, ...), so admitting one
/// dispatch per logical CPU saturates the machine exactly the way #14732
/// observed (five cooks on an 18-CPU host). A quarter of the logical CPU
/// count, floored at 1, leaves headroom for that burst while still using a
/// multi-core host for more than one concurrent cook.
pub fn concurrency_ceiling(cpu_count: usize) -> usize {
    (cpu_count / 4).max(1)
}

/// Decide whether another local Cook provider dispatch may start.
///
/// `severity` is the already-captured resource-policy severity (`"ok"`,
/// `"warm"`, or `"hot"`); any other value is treated as `"ok"` so an absent or
/// unrecognized snapshot fails open rather than blocking dispatch on a host
/// this decision cannot evaluate.
///
/// `local_override` is the resource-policy `--placement local` override
/// already recorded on the captured context. It is the one documented,
/// operator-authorized escape hatch from a hot-machine refusal elsewhere in
/// resource policy ("Local execution requires an explicit, authorized
/// `--placement local` override"), so a hot refusal here respects it too — an
/// explicit override is a decision the operator already made, not one this
/// ceiling second-guesses. The concurrency ceiling itself is a physical
/// constraint, not a pressure judgment call, so it still queues even under an
/// override: queueing delays the authorized run until a slot is free, it does
/// not refuse it.
pub fn evaluate_local_dispatch_admission(
    active_count: usize,
    severity: &str,
    load_one: Option<f64>,
    cpu_count: usize,
    local_override: bool,
) -> LocalDispatchAdmission {
    if severity == "hot" && !local_override {
        let load = load_one
            .map(|load| format!("load average {load:.1} across {cpu_count} CPU(s)"))
            .unwrap_or_else(|| format!("elevated load across {cpu_count} CPU(s)"));
        return LocalDispatchAdmission::Refuse {
            reason: format!(
                "machine is hot ({load}); refusing to start another local Cook provider dispatch"
            ),
        };
    }
    let ceiling = concurrency_ceiling(cpu_count);
    if active_count >= ceiling {
        LocalDispatchAdmission::Queue {
            active: active_count,
            ceiling,
        }
    } else {
        LocalDispatchAdmission::Admit
    }
}

fn lease_dir(data_root: &Path) -> PathBuf {
    homeboy_paths::local_cook_dispatch_leases_dir_in_root(data_root)
}

fn lease_path(data_root: &Path, run_id: &str) -> PathBuf {
    lease_dir(data_root).join(format!(
        "{}.json",
        homeboy_paths::sanitize_path_segment(run_id)
    ))
}

fn read_lease(path: &Path) -> Result<Option<LocalDispatchLease>> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&content).map(Some).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some(format!("parse local dispatch lease {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })
}

fn lease_files(data_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = lease_dir(data_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|error| Error::internal_io(error.to_string(), Some(dir.display().to_string())))?
    {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(dir.display().to_string()))
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Count of live local dispatch leases below `data_root`, pruning any lease
/// whose holder process is provably gone first. A dead holder never
/// permanently consumes a concurrency slot.
pub fn active_local_dispatch_count(data_root: &Path) -> Result<usize> {
    let mut count = 0;
    for path in lease_files(data_root)? {
        match read_lease(&path)? {
            Some(lease) if crate::process::pid_is_running(lease.pid) => count += 1,
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(count)
}

/// Unconditionally acquire a lease for `run_id` below `data_root`.
///
/// Callers must have already decided admission via
/// [`evaluate_local_dispatch_admission`]; this only records the slot so
/// [`active_local_dispatch_count`] can see it. The returned guard releases the
/// slot on drop.
pub fn acquire_local_dispatch_lease(
    data_root: &Path,
    run_id: &str,
) -> Result<ActiveLocalDispatchLease> {
    let dir = lease_dir(data_root);
    fs::create_dir_all(&dir)
        .map_err(|error| Error::internal_io(error.to_string(), Some(dir.display().to_string())))?;
    let pid = std::process::id();
    let path = lease_path(data_root, run_id);
    let lease = LocalDispatchLease {
        run_id: run_id.to_string(),
        pid,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&lease).map_err(|error| {
        Error::internal_unexpected(format!("failed to serialize local dispatch lease: {error}"))
    })?;
    fs::write(&path, json)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok(ActiveLocalDispatchLease { path, pid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_below_the_ceiling() {
        assert_eq!(
            evaluate_local_dispatch_admission(0, "ok", Some(1.0), 18, false),
            LocalDispatchAdmission::Admit
        );
        assert_eq!(
            evaluate_local_dispatch_admission(3, "warm", Some(10.0), 18, false),
            LocalDispatchAdmission::Admit
        );
    }

    /// The regression this module exists to fix: dispatching more cooks than
    /// the ceiling supports must queue the excess instead of running it.
    #[test]
    fn queues_once_the_ceiling_is_reached() {
        // 18 CPUs -> ceiling of 4 (18 / 4, floored at 1). A 5th concurrent
        // dispatch — exactly the reported scenario — must queue, not run.
        assert_eq!(concurrency_ceiling(18), 4);
        assert_eq!(
            evaluate_local_dispatch_admission(4, "warm", Some(30.4), 18, false),
            LocalDispatchAdmission::Queue {
                active: 4,
                ceiling: 4
            }
        );
        assert_eq!(
            evaluate_local_dispatch_admission(5, "ok", None, 18, false),
            LocalDispatchAdmission::Queue {
                active: 5,
                ceiling: 4
            }
        );
    }

    /// The other regression this module exists to fix: a hot machine is
    /// refused outright, with the load named in the reason, instead of
    /// warned-about and dispatched into anyway.
    #[test]
    fn refuses_admission_when_pressure_is_already_hot_and_names_the_load() {
        let admission = evaluate_local_dispatch_admission(0, "hot", Some(30.4), 18, false);
        let LocalDispatchAdmission::Refuse { reason } = admission else {
            panic!("expected a refusal, got {admission:?}");
        };
        assert!(reason.contains("hot"), "{reason}");
        assert!(reason.contains("30.4"), "{reason}");
        assert!(reason.contains("18 CPU"), "{reason}");
    }

    /// Refusal takes priority over queueing: a hot machine is refused even
    /// when there happens to be room under the concurrency ceiling, because
    /// the problem is pressure, not slot availability.
    #[test]
    fn hot_pressure_refuses_even_with_free_slots() {
        assert!(matches!(
            evaluate_local_dispatch_admission(0, "hot", Some(9.9), 18, false),
            LocalDispatchAdmission::Refuse { .. }
        ));
    }

    /// An explicit `--placement local` override is the one documented escape
    /// hatch from a hot-machine refusal elsewhere in resource policy, so it
    /// bypasses this refusal too. It does not bypass the concurrency ceiling:
    /// a physical constraint queues even an authorized run.
    #[test]
    fn explicit_local_override_bypasses_the_hot_refusal_but_not_the_ceiling() {
        assert_eq!(
            evaluate_local_dispatch_admission(0, "hot", Some(30.4), 18, true),
            LocalDispatchAdmission::Admit
        );
        assert_eq!(
            evaluate_local_dispatch_admission(4, "hot", Some(30.4), 18, true),
            LocalDispatchAdmission::Queue {
                active: 4,
                ceiling: 4
            }
        );
    }

    #[test]
    fn unrecognized_severity_fails_open_to_ceiling_evaluation() {
        assert_eq!(
            evaluate_local_dispatch_admission(0, "unknown", None, 4, false),
            LocalDispatchAdmission::Admit
        );
    }

    #[test]
    fn ceiling_never_drops_below_one() {
        assert_eq!(concurrency_ceiling(0), 1);
        assert_eq!(concurrency_ceiling(1), 1);
        assert_eq!(concurrency_ceiling(3), 1);
        assert_eq!(concurrency_ceiling(4), 1);
        assert_eq!(concurrency_ceiling(8), 2);
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

    /// A lease left behind by a process that is provably gone (a pid that
    /// cannot possibly be this test's own live process) does not permanently
    /// consume a slot: the next count prunes it.
    #[test]
    fn a_dead_holder_lease_is_pruned_and_frees_its_slot() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let dir = super::lease_dir(data_root);
        fs::create_dir_all(&dir).expect("create lease dir");
        let stale = LocalDispatchLease {
            run_id: "stale-run".to_string(),
            // pid 1 is respawned as long-lived (init/launchd) on every
            // platform this test runs on, so pick an implausible worker pid
            // whose liveness this test does not depend on ambient state:
            // u32::MAX is never a valid live process id.
            pid: u32::MAX,
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        fs::write(
            super::lease_path(data_root, &stale.run_id),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .expect("write stale lease");

        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
        assert!(
            !super::lease_path(data_root, &stale.run_id).is_file(),
            "stale lease must be pruned from disk"
        );
    }
}
