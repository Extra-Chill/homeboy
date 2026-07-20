//! Daemon lease staleness & freshness decision logic.
//!
//! Extracted from `daemon.rs`: the pure (no JobStore / HTTP-serving)
//! machinery that decides whether a daemon session lease is running, fresh,
//! reachable, or stale, and turns that into a `DaemonFreshnessReport`. This is
//! the cluster the daemon-recovery bug fixes keep landing in (#9128, #8967,
//! #8897), so isolating the staleness decision from the serving spine makes it
//! reviewable and the invariants explicit.
//!
//! `DaemonLeaseValidation` is the typed verdict the serving code consumes;
//! `validate_lease_file` is the entry point.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{
    current_binary_sha256, read_termination_evidence, runtime_path_fingerprint,
    DaemonFreshnessReport, DaemonRepairStep, DaemonRuntimeSnapshot, DaemonStaleReasonCode,
    DaemonState, DAEMON_LEASE_SCHEMA,
};
use crate::process::pid_is_running;
use crate::{build_identity, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonLeaseValidation {
    pub(crate) state: Option<DaemonState>,
    pub(crate) running: bool,
    pub(crate) fresh: bool,
    pub(crate) reachable: bool,
    pub(crate) stale_reason: Option<String>,
    pub(crate) stale_reason_code: Option<DaemonStaleReasonCode>,
    pub(crate) invalid_pid: Option<u32>,
}

pub(crate) fn daemon_state_identity(state_path: &Path, jobs_path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for (label, path) in [
        (b"daemon-state\0".as_slice(), state_path),
        (b"daemon-jobs\0".as_slice(), jobs_path),
    ] {
        hasher.update(label);
        match fs::read(path) {
            Ok(bytes) => {
                hasher.update([1]);
                hasher.update(bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update([0]),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(format!("read {}", path.display())),
                ))
            }
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn validate_lease_file(path: &Path) -> Result<DaemonLeaseValidation> {
    if !path.exists() {
        return Ok(DaemonLeaseValidation {
            running: false,
            fresh: false,
            reachable: false,
            stale_reason: None,
            stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
            state: None,
            invalid_pid: None,
        });
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", path.display()))))?;
    let state: DaemonState = match serde_json::from_str(&content) {
        Ok(state) => state,
        Err(error) => {
            return Ok(DaemonLeaseValidation {
                running: false,
                fresh: false,
                reachable: false,
                stale_reason: Some(format!("invalid daemon lease: {error}")),
                stale_reason_code: Some(DaemonStaleReasonCode::LeaseCorrupt),
                state: None,
                invalid_pid: serde_json::from_str::<serde_json::Value>(&content)
                    .ok()
                    .and_then(|value| value.get("pid").and_then(|pid| pid.as_u64()))
                    .and_then(|pid| u32::try_from(pid).ok()),
            });
        }
    };

    let running = pid_is_running(state.pid);
    // A dead owner is safe to recover even when its persisted schema was
    // omitted. Keeping the parsed lease identity makes explicit adoption
    // possible; a live invalid lease remains protected by schema validation.
    if !running {
        return Ok(stale_lease(
            state,
            false,
            DaemonStaleReasonCode::PidDead,
            "daemon lease pid is not running",
        ));
    }
    if state.schema != DAEMON_LEASE_SCHEMA {
        return Ok(stale_lease(
            state,
            true,
            DaemonStaleReasonCode::LeaseSchemaMismatch,
            "unsupported daemon lease schema",
        ));
    }
    let current_identity = build_identity::current();
    if state.build_identity.version != current_identity.version
        || state.build_identity.display != current_identity.display
    {
        return Ok(stale_lease(
            state,
            true,
            DaemonStaleReasonCode::VersionMismatch,
            "daemon build identity does not match current Homeboy binary",
        ));
    }
    if state.binary_sha256 != current_binary_sha256()? {
        return Ok(stale_lease(
            state,
            true,
            DaemonStaleReasonCode::BinaryHashMismatch,
            "daemon binary hash does not match current Homeboy binary",
        ));
    }
    if let Some(reason) = runtime_snapshot_stale_reason(&state.runtime_paths) {
        return Ok(stale_lease(
            state,
            true,
            DaemonStaleReasonCode::RuntimePathsDrift,
            reason,
        ));
    }

    Ok(DaemonLeaseValidation {
        running: true,
        fresh: true,
        reachable: true,
        state: Some(state),
        stale_reason: None,
        stale_reason_code: None,
        invalid_pid: None,
    })
}

pub(crate) fn stale_lease(
    state: DaemonState,
    running: bool,
    code: DaemonStaleReasonCode,
    reason: impl Into<String>,
) -> DaemonLeaseValidation {
    DaemonLeaseValidation {
        running,
        fresh: false,
        reachable: running,
        stale_reason: Some(reason.into()),
        stale_reason_code: Some(code),
        state: Some(state),
        invalid_pid: None,
    }
}

pub(crate) fn freshness_report_from_validation(
    validation: &DaemonLeaseValidation,
    active_jobs: usize,
) -> DaemonFreshnessReport {
    let state = validation.state.as_ref();
    let restartable = !validation.fresh
        && active_jobs == 0
        && !matches!(
            validation.stale_reason_code,
            Some(DaemonStaleReasonCode::LeaseCorrupt | DaemonStaleReasonCode::TransportUnreachable)
        );
    let repair_plan = if restartable {
        vec![
            DaemonRepairStep {
                code: "daemon_stop".to_string(),
                command: "homeboy daemon stop".to_string(),
            },
            DaemonRepairStep {
                code: "daemon_start".to_string(),
                command: "homeboy daemon start".to_string(),
            },
        ]
    } else {
        Vec::new()
    };
    DaemonFreshnessReport {
        fresh: validation.fresh,
        stale_reason_code: validation.stale_reason_code,
        restartable,
        lease_id: state.map(|state| state.lease_id.clone()),
        pid: state.map(|state| state.pid),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: state.and_then(|state| state.binary_sha256.clone()),
        daemon_version: state.map(|state| state.build_identity.version.clone()),
        daemon_build_identity: state.map(|state| state.build_identity.display.clone()),
        runtime_paths: state.map(|state| state.runtime_paths.clone()),
        active_jobs,
        termination_evidence: (!validation.running)
            .then(read_termination_evidence)
            .transpose()
            .ok()
            .flatten()
            .flatten(),
        repair_plan,
    }
}

pub(crate) fn runtime_snapshot_stale_reason(snapshot: &DaemonRuntimeSnapshot) -> Option<String> {
    let stale: Vec<_> = snapshot
        .paths
        .iter()
        .filter(|path| runtime_path_fingerprint(Path::new(&path.path)) != path.fingerprint)
        .map(|path| path.env.clone())
        .collect();
    if stale.is_empty() {
        None
    } else {
        Some(format!(
            "daemon runtime path fingerprints changed: {}",
            stale.join(", ")
        ))
    }
}
