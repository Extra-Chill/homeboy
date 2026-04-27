//! Active-run leases for mutating rig commands.
//!
//! These leases are local-machine guardrails. They prevent two concurrent rig
//! commands from mutating the same declared resources at the same time; they do
//! not represent the long-lived state of a materialized rig after `rig up`
//! exits.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

mod lock;

use super::expand::expand_resources;
use super::spec::{RigResourcesSpec, RigSpec};
use super::state::now_rfc3339;
use crate::error::{Error, Result, RigResourceConflictInfo};
use crate::paths;
use lock::LeaseIndexLock;

/// On-disk lease held by one active mutating rig command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigRunLease {
    pub rig_id: String,
    pub command: String,
    pub pid: u32,
    pub started_at: String,
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
    if let Some(conflict) = find_conflict(rig, &resources)? {
        return Err(Error::rig_resource_conflict(RigResourceConflictInfo {
            rig_id: rig.id.clone(),
            command: command.to_string(),
            resource_kind: conflict.resource_kind,
            resource_value: conflict.resource_value,
            held_by_rig: conflict.lease.rig_id,
            held_by_command: conflict.lease.command,
            held_by_pid: conflict.lease.pid,
            held_since: conflict.lease.started_at,
        }));
    }

    let pid = std::process::id();
    let lease = RigRunLease {
        rig_id: rig.id.clone(),
        command: command.to_string(),
        pid,
        started_at: now_rfc3339(),
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

fn find_conflict(rig: &RigSpec, resources: &RigResourcesSpec) -> Result<Option<ResourceConflict>> {
    for lease in live_leases()? {
        if lease.rig_id == rig.id {
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

fn prune_stale_leases() -> Result<()> {
    for path in lease_files()? {
        let Some(lease) = read_lease(&path)? else {
            continue;
        };
        if !pid_is_live(lease.pid) {
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
            if pid_is_live(lease.pid) {
                leases.push(lease);
            }
        }
    }
    Ok(leases)
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
    Ok(paths::rig_leases_dir()?.join(format!("{}.json", sanitize_id(rig_id))))
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn pid_is_live(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/lease_test.rs"]
mod lease_test;
