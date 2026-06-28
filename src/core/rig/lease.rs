//! Active-run leases for mutating rig commands.
//!
//! These leases are local-machine guardrails. They prevent two concurrent rig
//! commands from mutating the same declared resources at the same time; they do
//! not represent the long-lived state of a materialized rig after `rig up`
//! exits.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod lock;

/// Environment variable that, when set to a positive integer number of seconds,
/// enables time-to-live based reclaim of rig run leases. A lease whose holder is
/// still alive but has been held longer than this TTL is treated as stale and
/// becomes reclaimable on the next acquire. Unset (the default) means leases are
/// only reclaimed when their holder process is provably gone — a live, recent
/// holder is never reclaimed automatically.
pub const RIG_LEASE_TTL_ENV: &str = "HOMEBOY_RIG_LEASE_TTL_SECS";

use super::expand::expand_resources;
use super::spec::{RigResourcesSpec, RigSpec};
use super::state::now_rfc3339;
use crate::core::error::{Error, Result, RigResourceConflictInfo};
use crate::core::paths;
use lock::LeaseIndexLock;

/// On-disk lease held by one active mutating rig command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigRunLease {
    pub rig_id: String,
    pub command: String,
    pub pid: u32,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    pub resources: RigResourcesSpec,
}

/// RAII guard that removes the lease when the command exits normally or with an
/// error. Process crashes are handled by stale-PID cleanup on the next acquire.
#[derive(Debug)]
pub struct ActiveRigRunLease {
    rig_id: String,
    pid: u32,
}

impl Drop for ActiveRigRunLease {
    fn drop(&mut self) {
        let Ok(_lock) = LeaseIndexLock::acquire() else {
            return;
        };
        let Ok(path) = lease_path(&self.rig_id) else {
            return;
        };
        let Ok(Some(lease)) = read_lease(&path) else {
            return;
        };
        if lease.pid == self.pid {
            let _ = fs::remove_file(path);
        }
    }
}

/// Acquire an active-run lease for a mutating rig command.
pub fn acquire_active_run_lease(rig: &RigSpec, command: &str) -> Result<Option<ActiveRigRunLease>> {
    let resources = expand_resources(rig);
    if resources.is_empty() {
        return Ok(None);
    }

    let _lock = LeaseIndexLock::acquire()?;
    fs::create_dir_all(paths::rig_leases_dir()?).map_err(|e| {
        Error::internal_unexpected(format!("Failed to create rig lease directory: {}", e))
    })?;

    prune_stale_leases()?;
    if has_covering_parent_lease(rig, command)? {
        return Ok(None);
    }
    if let Some(conflict) = find_conflict(rig, command, &resources)? {
        let held_age_seconds = lease_age_seconds(&conflict.lease.started_at);
        return Err(Error::rig_resource_conflict(RigResourceConflictInfo {
            rig_id: rig.id.clone(),
            command: command.to_string(),
            resource_kind: conflict.resource_kind,
            resource_value: conflict.resource_value,
            held_by_rig: conflict.lease.rig_id,
            held_by_command: conflict.lease.command,
            held_by_pid: conflict.lease.pid,
            held_since: conflict.lease.started_at,
            held_by_run_id: conflict.lease.run_id,
            held_by_runner_id: conflict.lease.runner_id,
            held_age_seconds,
        }));
    }

    let pid = std::process::id();
    let lease = RigRunLease {
        rig_id: rig.id.clone(),
        command: command.to_string(),
        pid,
        started_at: now_rfc3339(),
        run_id: active_run_id(),
        runner_id: active_lab_runner_id(),
        resources,
    };
    let json = serde_json::to_string_pretty(&lease)
        .map_err(|e| Error::internal_unexpected(format!("Failed to serialize rig lease: {}", e)))?;
    fs::write(lease_path(&rig.id)?, json).map_err(|e| {
        Error::internal_unexpected(format!("Failed to write rig lease for '{}': {}", rig.id, e))
    })?;

    Ok(Some(ActiveRigRunLease {
        rig_id: rig.id.clone(),
        pid,
    }))
}

struct ResourceConflict {
    lease: RigRunLease,
    resource_kind: String,
    resource_value: String,
}

fn find_conflict(
    rig: &RigSpec,
    command: &str,
    resources: &RigResourcesSpec,
) -> Result<Option<ResourceConflict>> {
    for lease in live_leases()? {
        if lease.rig_id == rig.id {
            if lease_allows_child_command(&lease, command) {
                continue;
            }
            return Ok(Some(ResourceConflict {
                resource_kind: "rig".to_string(),
                resource_value: rig.id.clone(),
                lease,
            }));
        }
        if let Some((kind, value)) = overlapping_resource(resources, &lease.resources) {
            return Ok(Some(ResourceConflict {
                lease,
                resource_kind: kind,
                resource_value: value,
            }));
        }
    }
    Ok(None)
}

fn has_covering_parent_lease(rig: &RigSpec, command: &str) -> Result<bool> {
    Ok(live_leases()?
        .iter()
        .any(|lease| lease.rig_id == rig.id && lease_allows_child_command(lease, command)))
}

fn lease_allows_child_command(lease: &RigRunLease, command: &str) -> bool {
    lease.pid == std::process::id() && lease.command == "trace compare" && command == "trace"
}

fn active_run_id() -> Option<String> {
    non_empty_env(crate::core::observation::ACTIVE_RUN_ID_ENV)
        .or_else(|| non_empty_env("HOMEBOY_RUN_ID"))
        .or_else(|| lab_metadata_string("run_id"))
        .or_else(|| lab_metadata_string_pointer("/proof/provenance/run_id"))
}

fn active_lab_runner_id() -> Option<String> {
    lab_metadata_string("runner_id")
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn lab_metadata_string(key: &str) -> Option<String> {
    lab_metadata_value()
        .and_then(|metadata| {
            metadata
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty())
}

fn lab_metadata_string_pointer(pointer: &str) -> Option<String> {
    lab_metadata_value()
        .and_then(|metadata| {
            metadata
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty())
}

fn lab_metadata_value() -> Option<serde_json::Value> {
    std::env::var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
}

fn overlapping_resource(
    wanted: &RigResourcesSpec,
    held: &RigResourcesSpec,
) -> Option<(String, String)> {
    for token in &wanted.exclusive {
        if held.exclusive.contains(token) {
            return Some(("exclusive".to_string(), token.clone()));
        }
    }
    for port in &wanted.ports {
        if held.ports.contains(port) {
            return Some(("port".to_string(), port.to_string()));
        }
    }
    for pattern in &wanted.process_patterns {
        if held.process_patterns.contains(pattern) {
            return Some(("process_pattern".to_string(), pattern.clone()));
        }
    }
    for wanted_path in &wanted.paths {
        for held_path in &held.paths {
            if paths_overlap(wanted_path, held_path) {
                return Some(("path".to_string(), wanted_path.clone()));
            }
        }
    }
    None
}

fn paths_overlap(a: &str, b: &str) -> bool {
    let a = Path::new(a);
    let b = Path::new(b);
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Read the configured lease TTL, if any. Returns `None` when the env var is
/// unset, empty, non-numeric, or zero — in which case TTL-based reclaim is
/// disabled and only dead holders are reclaimed.
fn lease_ttl() -> Option<Duration> {
    std::env::var(RIG_LEASE_TTL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
}

/// Wall-clock age of a lease in seconds derived from its `started_at` timestamp.
/// Returns `None` if the timestamp cannot be parsed.
fn lease_age_seconds(started_at: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let age = chrono::Utc::now().signed_duration_since(started.with_timezone(&chrono::Utc));
    Some(age.num_seconds())
}

/// A lease is reclaimable when its holder process is provably gone, or — when a
/// TTL is configured — when it has been held longer than that TTL. A live holder
/// within its TTL window is never reclaimable: we never steal an active lock.
fn lease_is_reclaimable(lease: &RigRunLease) -> bool {
    if !crate::core::process::pid_is_running(lease.pid) {
        return true;
    }
    if let Some(ttl) = lease_ttl() {
        if let Some(age) = lease_age_seconds(&lease.started_at) {
            return age >= 0 && (age as u64) > ttl.as_secs();
        }
    }
    false
}

fn prune_stale_leases() -> Result<()> {
    for path in lease_files()? {
        let Some(lease) = read_lease(&path)? else {
            continue;
        };
        if lease_is_reclaimable(&lease) {
            fs::remove_file(&path).map_err(|e| {
                Error::internal_unexpected(format!(
                    "Failed to remove stale rig lease {}: {}",
                    path.display(),
                    e
                ))
            })?;
        }
    }
    Ok(())
}

fn live_leases() -> Result<Vec<RigRunLease>> {
    let mut leases = Vec::new();
    for path in lease_files()? {
        if let Some(lease) = read_lease(&path)? {
            if !lease_is_reclaimable(&lease) {
                leases.push(lease);
            }
        }
    }
    Ok(leases)
}

/// Outcome of an operator-initiated rig lock release.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseLeaseOutcome {
    /// No lease was held for this rig.
    NoLease { rig_id: String },
    /// A lease was released.
    Released {
        rig_id: String,
        /// The released lease.
        lease: RigRunLease,
        /// Wall-clock age of the released lease in seconds, when derivable.
        age_seconds: Option<i64>,
        /// Whether the holder was already dead/stale (safe release) vs. a live
        /// holder forcibly reclaimed via `--force`.
        was_reclaimable: bool,
        /// Whether `--force` was used to override a live holder.
        forced: bool,
    },
}

/// Release the active run lease held for `rig_id`.
///
/// Without `force`, a lease is only released when its holder is provably gone or
/// past its configured TTL — releasing a live, recent holder requires `force`.
/// Releasing the lock does not terminate the holder process; it only frees the
/// local guardrail so a new run can proceed. With `force`, the caller is
/// asserting the holder is dead or wedged.
pub fn release_active_run_lease(rig_id: &str, force: bool) -> Result<ReleaseLeaseOutcome> {
    let _lock = LeaseIndexLock::acquire()?;
    let path = lease_path(rig_id)?;
    let Some(lease) = read_lease(&path)? else {
        return Ok(ReleaseLeaseOutcome::NoLease {
            rig_id: rig_id.to_string(),
        });
    };

    let was_reclaimable = lease_is_reclaimable(&lease);
    let age_seconds = lease_age_seconds(&lease.started_at);
    if !was_reclaimable && !force {
        return Err(Error::rig_resource_conflict(RigResourceConflictInfo {
            rig_id: rig_id.to_string(),
            command: "release-lock".to_string(),
            resource_kind: "rig".to_string(),
            resource_value: rig_id.to_string(),
            held_by_rig: lease.rig_id.clone(),
            held_by_command: lease.command.clone(),
            held_by_pid: lease.pid,
            held_since: lease.started_at.clone(),
            held_by_run_id: lease.run_id.clone(),
            held_by_runner_id: lease.runner_id.clone(),
            held_age_seconds: age_seconds,
        }));
    }

    fs::remove_file(&path).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to release rig lease for '{}': {}",
            rig_id, e
        ))
    })?;

    Ok(ReleaseLeaseOutcome::Released {
        rig_id: rig_id.to_string(),
        lease,
        age_seconds,
        was_reclaimable,
        forced: force && !was_reclaimable,
    })
}

/// List active rig run leases without acquiring or mutating leases.
pub fn active_run_leases() -> Result<Vec<RigRunLease>> {
    live_leases()
}

fn lease_files() -> Result<Vec<PathBuf>> {
    let dir = paths::rig_leases_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| {
        Error::internal_unexpected(format!("Failed to read rig lease directory: {}", e))
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_unexpected(format!("Failed to read rig lease entry: {}", e))
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_lease(path: &Path) -> Result<Option<RigRunLease>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to read rig lease {}: {}",
            path.display(),
            e
        ))
    })?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&content).map(Some).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse rig lease {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })
}

fn lease_path(rig_id: &str) -> Result<PathBuf> {
    Ok(paths::rig_leases_dir()?.join(format!("{}.json", paths::sanitize_path_segment(rig_id))))
}

#[cfg(test)]
#[path = "../../../tests/core/rig/lease_test.rs"]
mod lease_test;
