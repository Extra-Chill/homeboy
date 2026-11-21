//! Host-wide admission for local deterministic gates (#18555).
//!
//! Every Homeboy process on a host that shares one data root queues its local
//! gates through one registry. Ranking reuses the Lab step queue policy
//! ([`crate::lab_capacity::admission_order`]): priority lane, then the
//! least-occupied owner, then arrival, with the owner share cap while another
//! owner waits. Leases are OS file locks, so a killed process cannot strand a
//! slot or a queue entry.
//!
//! The Lab daemon on the same host reads [`counted_holders`] and charges those
//! gates against its step budget and its runner-child capacity, so Cooks run
//! with `--placement local` on a Lab host and remote work admitted by that
//! daemon draw from one slot pool instead of two. Gates of a Cook the daemon
//! already supervises (a step or runner child) are not charged twice.
use crate::lab_capacity::{
    admission_order, eta_upper_bound_seconds, owner_max_share_percent, owner_slot_cap, LabPriority,
    LabWaiter,
};
use crate::{Error, Result};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Positive slot limit for local gates on this host.
pub const LOCAL_GATE_SLOTS_ENV: &str = "HOMEBOY_LOCAL_GATE_SLOTS";
/// Positive one-minute load at which local gates stop being admitted while
/// another local gate already holds a slot.
pub const LOCAL_GATE_MAX_LOAD_ENV: &str = "HOMEBOY_LOCAL_GATE_MAX_LOAD";
const REGISTRY_DIR: &str = "local-gate-admission";

/// Who is asking for a local gate slot.
#[derive(Debug, Clone)]
pub struct LocalGateClaim {
    /// Fairness unit: the workflow instance when the Cook runs inside one,
    /// otherwise the Cook.
    pub owner: String,
    pub cook_id: String,
    /// Public gate label (`gate-<index>`); private command text never lands here.
    pub gate: String,
    pub priority: LabPriority,
    /// Declared run time, used only for waiters' ETA estimates.
    pub budget: Option<Duration>,
    /// Daemon job (workflow step or runner child) supervising the Cook. That
    /// job already holds a daemon slot, so the daemon must not charge this
    /// gate a second time.
    pub daemon_job: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    ticket: String,
    #[serde(default)]
    owner: String,
    cook_id: String,
    gate: String,
    #[serde(default)]
    priority: LabPriority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    daemon_job: Option<String>,
    queued_at_ms: u64,
    started_at_ms: Option<u64>,
}

impl Entry {
    fn owner(&self) -> &str {
        if self.owner.is_empty() {
            &self.cook_id
        } else {
            &self.owner
        }
    }

    fn remaining(&self, now: u64) -> Option<Duration> {
        let budget = Duration::from_secs(self.budget_seconds?);
        let elapsed = Duration::from_millis(now.saturating_sub(self.started_at_ms.unwrap_or(now)));
        Some(budget.saturating_sub(elapsed))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn io(error: std::io::Error) -> Error {
    Error::internal_io(error.to_string(), Some("local gate admission".into()))
}

/// The registry this process shares with the rest of the host. `None` under
/// test isolation, where a host-wide queue would couple unrelated tests.
fn host_root() -> Result<Option<PathBuf>> {
    if cfg!(test) || crate::paths::hermetic_runtime_root().is_some() {
        return Ok(None);
    }
    Ok(Some(crate::paths::homeboy_data()?.join(REGISTRY_DIR)))
}

/// One-minute load average, when the host publishes one.
pub fn load_average_1m() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
}

pub struct Registry {
    root: PathBuf,
    limit: usize,
    max_load: f64,
}

impl Registry {
    /// The host registry with operator configuration, or `None` under test
    /// isolation.
    pub fn host() -> Result<Option<Self>> {
        let Some(root) = host_root()? else {
            return Ok(None);
        };
        Self::configured(root).map(Some)
    }

    fn configured(root: PathBuf) -> Result<Self> {
        let cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let limit = std::env::var(LOCAL_GATE_SLOTS_ENV)
            .ok()
            .map(|value| {
                value
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| *limit > 0)
                    .ok_or_else(|| {
                        Error::invalid_argument(LOCAL_GATE_SLOTS_ENV, "expected a positive integer")
                    })
            })
            .transpose()?
            .unwrap_or((cpus / 2).max(1));
        let max_load = std::env::var(LOCAL_GATE_MAX_LOAD_ENV)
            .ok()
            .map(|value| {
                value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|load| load.is_finite() && *load > 0.0)
                    .ok_or_else(|| {
                        Error::invalid_argument(
                            LOCAL_GATE_MAX_LOAD_ENV,
                            "expected a positive finite load",
                        )
                    })
            })
            .transpose()?
            .unwrap_or(cpus as f64);
        Ok(Self {
            root,
            limit,
            max_load,
        })
    }

    fn transaction<T>(
        &self,
        own: Option<&str>,
        update: impl FnOnce(&mut Vec<Entry>) -> T,
    ) -> Result<T> {
        fs::create_dir_all(&self.root).map_err(io)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("registry.lock"))
            .map_err(io)?;
        lock.lock_exclusive().map_err(io)?;
        let state = self.root.join("queue.json");
        let mut entries: Vec<Entry> = match fs::read(&state) {
            // A torn or foreign file must not wedge every local gate.
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => return Err(io(error)),
        };
        entries.retain(|entry| {
            if own == Some(entry.ticket.as_str()) {
                return true;
            }
            let path = self.root.join(format!("{}.lease", entry.ticket));
            match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(lease) => match lease.try_lock_exclusive() {
                    Ok(true) => {
                        let _ = fs::remove_file(path);
                        false
                    }
                    Ok(false) => true,
                    Err(_) => true, // Never oversubscribe on an uncertain lease.
                },
                Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            }
        });
        let result = update(&mut entries);
        let bytes = serde_json::to_vec(&entries)
            .map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let tmp = self.root.join("queue.tmp");
        fs::write(&tmp, bytes).map_err(io)?;
        fs::rename(tmp, state).map_err(io)?;
        Ok(result)
    }

    fn pressure(&self, load: Option<f64>) -> serde_json::Value {
        serde_json::json!({
            "load_average_1m": load,
            "max_load": self.max_load,
            "blocked": load.is_some_and(|load| load >= self.max_load),
        })
    }

    /// Join the queue. The returned permit holds its place until admitted or
    /// dropped.
    pub fn enqueue(self, claim: LocalGateClaim) -> Result<LocalGatePermit> {
        fs::create_dir_all(&self.root).map_err(io)?;
        let ticket = uuid::Uuid::new_v4().to_string();
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(self.root.join(format!("{ticket}.lease")))
            .map_err(io)?;
        lease.lock_exclusive().map_err(io)?;
        self.transaction(Some(&ticket), |entries| {
            entries.push(Entry {
                ticket: ticket.clone(),
                owner: claim.owner,
                cook_id: claim.cook_id,
                gate: claim.gate,
                priority: claim.priority,
                budget_seconds: claim.budget.map(|budget| budget.as_secs().max(1)),
                daemon_job: claim.daemon_job,
                queued_at_ms: now_ms(),
                started_at_ms: None,
            })
        })?;
        Ok(LocalGatePermit {
            registry: self,
            ticket,
            _lease: lease,
        })
    }
}

/// Waiter tickets ranked head first, by the Lab step queue policy.
fn ranked_waiters(entries: &[Entry], limit: usize) -> (Vec<usize>, usize) {
    let mut held = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.started_at_ms.is_some()) {
        *held.entry(entry.owner().to_string()).or_insert(0usize) += 1;
    }
    let waiters = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.started_at_ms.is_none())
        .map(|(index, entry)| LabWaiter {
            ticket: index,
            owner: entry.owner().to_string(),
            priority: entry.priority,
            // Entries are already in enqueue order; the index breaks ties.
            arrived_ms: 0,
        })
        .collect::<Vec<_>>();
    let cap = owner_slot_cap(limit, owner_max_share_percent());
    (admission_order(&waiters, &held, cap), cap)
}

/// A place in the host-wide local gate queue; once admitted, a held slot.
pub struct LocalGatePermit {
    registry: Registry,
    ticket: String,
    _lease: File,
}

impl LocalGatePermit {
    /// Admit when this waiter heads the ranked queue and a slot is free. Load
    /// at or above the threshold holds the queue only while another local gate
    /// runs, so one gate always makes forward progress.
    pub fn try_admit(&self, load: Option<f64>) -> Result<bool> {
        let blocked = load.is_some_and(|load| load >= self.registry.max_load);
        let limit = self.registry.limit;
        self.registry.transaction(Some(&self.ticket), |entries| {
            let active = entries
                .iter()
                .filter(|entry| entry.started_at_ms.is_some())
                .count();
            let own = entries.iter().position(|entry| entry.ticket == self.ticket);
            let (order, _) = ranked_waiters(entries, limit);
            let held_back = blocked && active > 0;
            match own {
                Some(index) if !held_back && active < limit && order.first() == Some(&index) => {
                    entries[index].started_at_ms = Some(now_ms());
                    true
                }
                _ => false,
            }
        })
    }

    /// Wait for admission, polling `cancelled`. Returns the time spent queued,
    /// or `None` when cancelled first (the queue entry is released on drop).
    pub fn wait(&self, cancelled: impl Fn() -> bool) -> Result<Option<Duration>> {
        let started = Instant::now();
        loop {
            if cancelled() {
                return Ok(None);
            }
            if self.try_admit(load_average_1m())? {
                return Ok(Some(started.elapsed()));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for LocalGatePermit {
    fn drop(&mut self) {
        let _ = self.registry.transaction(Some(&self.ticket), |entries| {
            entries.retain(|entry| entry.ticket != self.ticket)
        });
        let _ = fs::remove_file(self.registry.root.join(format!("{}.lease", self.ticket)));
    }
}

/// Live host view shared by `agent-task capacity` and `agent-task status`;
/// needs no daemon transport. Field names follow the Lab step queue snapshot.
pub fn snapshot() -> Result<serde_json::Value> {
    match Registry::host()? {
        Some(registry) => snapshot_for(&registry, load_average_1m()),
        None => Ok(serde_json::json!({"available": false, "reason": "test isolation"})),
    }
}

fn snapshot_for(registry: &Registry, load: Option<f64>) -> Result<serde_json::Value> {
    let limit = registry.limit;
    let pressure = registry.pressure(load);
    let entries = &live_entries(&registry.root);
    let now = now_ms();
    let (order, cap) = ranked_waiters(entries, limit);
    let holders = entries
        .iter()
        .filter(|entry| entry.started_at_ms.is_some())
        .collect::<Vec<_>>();
    let remaining = holders
        .iter()
        .map(|entry| entry.remaining(now))
        .collect::<Vec<_>>();
    let holder_values = holders
        .iter()
        .map(|entry| {
            serde_json::json!({
                "owner": entry.owner(), "cook_id": entry.cook_id, "gate": entry.gate,
                "priority": entry.priority,
                "elapsed_seconds": now.saturating_sub(entry.started_at_ms.unwrap_or(now)) / 1000,
                "budget_seconds": entry.budget_seconds,
                "daemon_slot": entry.daemon_job.is_some(),
            })
        })
        .collect::<Vec<_>>();
    let waiting = order
        .iter()
        .enumerate()
        .map(|(index, ticket)| {
            let entry = &entries[*ticket];
            serde_json::json!({
                "queue_position": index + 1, "owner": entry.owner(), "cook_id": entry.cook_id,
                "gate": entry.gate, "priority": entry.priority,
                "waited_seconds": now.saturating_sub(entry.queued_at_ms) / 1000,
                "eta_seconds": eta_upper_bound_seconds(&remaining, index + 1, limit),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "available": true,
        "active_slots": holders.len(),
        "free_slots": limit.saturating_sub(holders.len()),
        "slot_limit": limit,
        "owner_max_share_percent": owner_max_share_percent(),
        "owner_slot_cap": cap,
        "lab_charged_holders": count_charged(entries),
        "holders": holder_values,
        "waiting": waiting,
        "eta_kind": "upper_bound_from_declared_budgets",
        "pressure": pressure,
    }))
}

fn count_charged(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.started_at_ms.is_some() && entry.daemon_job.is_none())
        .count()
}

/// Local gates holding a host slot that no daemon step slot already covers.
/// The Lab daemon charges these against its step and runner admission
/// budgets. Read-only and lock-free (the registry is replaced atomically), so
/// admission loops can poll it; entries whose lease is no longer held are not
/// counted. Best effort: an unreadable registry charges nothing rather than
/// wedging the daemon.
pub fn counted_holders() -> usize {
    let Ok(Some(root)) = host_root() else {
        return 0;
    };
    counted_holders_in(&root)
}

fn counted_holders_in(root: &std::path::Path) -> usize {
    count_charged(&live_entries(root))
}

/// The registry's live entries, read without its lock or any write: the queue
/// file is replaced atomically, and an entry whose lease this probe can take
/// belongs to a dead process. Read-only views (`status`, `capacity`, the Lab
/// daemon's charge) use this; only queue members prune through
/// [`Registry::transaction`].
fn live_entries(root: &std::path::Path) -> Vec<Entry> {
    let Ok(bytes) = fs::read(root.join("queue.json")) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = serde_json::from_slice(&bytes).unwrap_or_default();
    entries.retain(
        |entry| match File::open(root.join(format!("{}.lease", entry.ticket))) {
            Ok(lease) => !matches!(FileExt::try_lock_shared(&lease), Ok(true)),
            Err(_) => false,
        },
    );
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(owner: &str, cook: &str, gate: &str) -> LocalGateClaim {
        LocalGateClaim {
            owner: owner.into(),
            cook_id: cook.into(),
            gate: gate.into(),
            priority: LabPriority::Normal,
            budget: None,
            daemon_job: None,
        }
    }

    fn registry(root: &std::path::Path, limit: usize) -> Registry {
        Registry {
            root: root.into(),
            limit,
            max_load: 4.0,
        }
    }

    #[test]
    fn local_capacity_admission_queues_and_releases_fifo() {
        let root = tempfile::tempdir().unwrap();
        let first = registry(root.path(), 1)
            .enqueue(claim("cook-a", "cook-a", "gate-1"))
            .unwrap();
        assert!(first.try_admit(None).unwrap());
        let second = registry(root.path(), 1)
            .enqueue(claim("cook-b", "cook-b", "gate-2"))
            .unwrap();
        assert!(!second.try_admit(None).unwrap());
        let snapshot = snapshot_for(&registry(root.path(), 1), None).unwrap();
        assert_eq!(snapshot["active_slots"], 1);
        assert_eq!(snapshot["free_slots"], 0);
        assert_eq!(snapshot["waiting"][0]["cook_id"], "cook-b");
        assert_eq!(snapshot["waiting"][0]["queue_position"], 1);
        assert_eq!(snapshot["holders"][0]["cook_id"], "cook-a");
        drop(first);
        assert!(second.try_admit(None).unwrap());
        assert_eq!(
            snapshot_for(&registry(root.path(), 1), None).unwrap()["holders"][0]["cook_id"],
            "cook-b"
        );
    }

    #[test]
    fn local_capacity_admission_pressure_holds_only_behind_a_running_gate() {
        let root = tempfile::tempdir().unwrap();
        let first = registry(root.path(), 2)
            .enqueue(claim("a", "a", "gate-1"))
            .unwrap();
        assert!(
            first.try_admit(Some(100.0)).unwrap(),
            "an idle host always admits one gate"
        );
        let second = registry(root.path(), 2)
            .enqueue(claim("b", "b", "gate-1"))
            .unwrap();
        assert!(
            !second.try_admit(Some(100.0)).unwrap(),
            "pressure holds a free slot"
        );
        assert!(second.try_admit(Some(1.0)).unwrap());
    }

    #[test]
    fn local_capacity_admission_caps_one_owner_while_another_waits() {
        let root = tempfile::tempdir().unwrap();
        let held = registry(root.path(), 2)
            .enqueue(claim("workflow", "cook-1", "gate-1"))
            .unwrap();
        assert!(held.try_admit(None).unwrap());
        let same_owner = registry(root.path(), 2)
            .enqueue(claim("workflow", "cook-2", "gate-1"))
            .unwrap();
        let other = registry(root.path(), 2)
            .enqueue(claim("remote", "cook-3", "gate-1"))
            .unwrap();
        assert!(
            !same_owner.try_admit(None).unwrap(),
            "an owner at its share cap yields to another waiting owner"
        );
        assert!(other.try_admit(None).unwrap());
    }

    #[test]
    fn local_capacity_admission_reaps_abandoned_leases() {
        let root = tempfile::tempdir().unwrap();
        let first = registry(root.path(), 1)
            .enqueue(claim("dead", "dead", "gate-1"))
            .unwrap();
        assert!(first.try_admit(None).unwrap());
        // Simulate process exit without the permit's drop.
        FileExt::unlock(&first._lease).unwrap();
        let next = registry(root.path(), 1)
            .enqueue(claim("live", "live", "gate-2"))
            .unwrap();
        assert!(next.try_admit(None).unwrap());
    }

    #[test]
    fn lab_charges_only_gates_without_a_step_slot() {
        let root = tempfile::tempdir().unwrap();
        let bare = registry(root.path(), 4)
            .enqueue(claim("bare", "bare", "gate-1"))
            .unwrap();
        assert!(bare.try_admit(None).unwrap());
        let mut stepped = claim("wf", "stepped", "gate-1");
        stepped.daemon_job = Some("job-1".into());
        let stepped = registry(root.path(), 4).enqueue(stepped).unwrap();
        assert!(stepped.try_admit(None).unwrap());
        let snapshot = snapshot_for(&registry(root.path(), 4), None).unwrap();
        assert_eq!(snapshot["active_slots"], 2);
        assert_eq!(snapshot["lab_charged_holders"], 1);
        assert_eq!(counted_holders_in(root.path()), 1);
        drop(bare);
        assert_eq!(counted_holders_in(root.path()), 0);
    }
}
