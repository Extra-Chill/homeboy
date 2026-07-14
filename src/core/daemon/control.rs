//! Local daemon lifecycle and artifact-fetch orchestration owned by core.
//!
//! The command layer (`src/commands/daemon.rs`) stays a thin adapter: it parses
//! arguments and renders output. The process spawning, status polling, HTTP
//! artifact fetch, and filesystem persistence live here so the orchestration is
//! testable and reusable outside the CLI.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::error::{Error, Result};
use crate::core::execution_contract::encode_uri_component;
use crate::core::process::pid_is_running;

use super::{
    acquire_daemon_operation_lock, acquire_daemon_operation_lock_for_ensure, parse_bind_addr,
    read_status, repair_legacy_lease_for_start, stop_unlocked, try_acquire_daemon_owner_lock,
    DaemonLeaselessOrphanReconciliationResult, DaemonLeaselessRecoveryResult,
    DaemonOrphanAdoptionResult, DaemonStaleReasonCode, DaemonStartResult, DAEMON_STARTUP_TOKEN_ENV,
};

/// Outcome of a daemon byte-endpoint artifact download.
#[derive(Debug, Clone)]
pub struct ArtifactFetchOutcome {
    pub daemon_url: String,
    pub content_url: String,
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

/// Recover active jobs from an absent daemon-state record only when an operator
/// supplies the exact lease and dead PID recorded before control-plane loss.
pub fn recover_missing_lease_state(
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: &str,
    confirm_pid_dead: bool,
    confirm_control_plane_lost: bool,
    addr: &str,
) -> Result<super::DaemonStateLossRecoveryResult> {
    if !confirm_pid_dead || !confirm_control_plane_lost {
        return Err(Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "state-loss recovery requires --confirm-pid-dead and --confirm-control-plane-lost",
            Some(lease_id.to_string()),
            None,
        ));
    }
    parse_bind_addr(addr)?;
    let recorded_endpoint = parse_recorded_daemon_endpoint(recorded_endpoint)?;
    let _lock = acquire_daemon_operation_lock()?;
    let receipt_path = crate::core::paths::daemon_state_loss_recovery_receipt_file(lease_id)?;
    let status = read_status()?;
    let existing = read_state_loss_receipt(&receipt_path)?;
    if let Some(receipt) = existing.as_ref() {
        validate_state_loss_receipt(receipt, lease_id, recorded_pid, recorded_endpoint)?;
        if receipt.phase == StateLossRecoveryPhase::ReplacementStarted {
            return receipt.clone().into_result();
        }
    }
    let endpoint_probe = validate_state_loss_preconditions(
        lease_id,
        recorded_pid,
        recorded_endpoint,
        &status,
        existing.as_ref(),
    )?;
    if let Some(mut receipt) = existing {
        let owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "lease_id",
                "daemon owner lock is held; refusing state-loss recovery",
                Some(lease_id.to_string()),
                None,
            )
        })?;
        if receipt.phase == StateLossRecoveryPhase::Prepared {
            drop(owner_lock);
            return Err(Error::validation_invalid_argument(
                "lease_id",
                "state-loss receipt is prepared but reconciliation did not complete; inspect the durable jobs before retrying",
                Some(lease_id.to_string()),
                None,
            ));
        }
        drop(owner_lock);
        complete_state_loss_replacement(&mut receipt, &receipt_path, || {
            start_or_return_live_unlocked(addr)
        })
    } else {
        let owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "lease_id",
                "daemon owner lock is held; refusing state-loss recovery",
                Some(lease_id.to_string()),
                None,
            )
        })?;
        let jobs_path = crate::core::paths::daemon_jobs_file()?;
        let raw_store = read_job_store_bytes(&jobs_path)?;
        let snapshot_path = snapshot_job_store(&jobs_path, &raw_store)?;
        let store =
            super::JobStore::open_without_reconciliation_from_bytes(&jobs_path, &raw_store)?;
        let diagnostics = store.daemon_lease_job_diagnostics(lease_id);
        if diagnostics.unowned_count() > 0
            || !diagnostics.other_lease_job_ids.is_empty()
            || diagnostics.matching_count() == 0
        {
            return Err(Error::validation_invalid_argument(
                "lease_id",
                "active durable jobs are not exclusively owned by the exact recovery lease",
                Some(lease_id.to_string()),
                None,
            ));
        }
        let mut receipt = StateLossRecoveryReceipt {
            lease_id: lease_id.to_string(),
            recorded_pid,
            recorded_endpoint: recorded_endpoint.to_string(),
            affected_job_ids: diagnostics.matching_job_ids.clone(),
            evidence_snapshot_path: snapshot_path.display().to_string(),
            ownership_proof: vec![
                format!("operator supplied exact missing lease `{lease_id}`"),
                format!("recorded daemon PID `{recorded_pid}` was not running"),
                "daemon owner lock acquired non-destructively".to_string(),
                endpoint_probe,
            ],
            phase: StateLossRecoveryPhase::Prepared,
            replacement: None,
        };
        write_state_loss_receipt(&receipt_path, &receipt)?;
        let reconciled = store.reconcile_dead_daemon_lease_jobs(lease_id)?;
        if reconciled.protected_count() > 0 {
            let _ = std::fs::remove_file(&receipt_path);
            return Err(Error::validation_invalid_argument(
                "lease_id",
                format!(
                    "deferred missing-lease recovery because {} active child process(es) are still running: {}",
                    reconciled.protected_count(),
                    reconciled.protected_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                ),
                Some(lease_id.to_string()),
                Some(vec!["Wait for the recorded child process to finish, then retry recovery.".to_string()]),
            ));
        }
        receipt.phase = StateLossRecoveryPhase::Reconciled;
        write_state_loss_receipt(&receipt_path, &receipt)?;
        drop(owner_lock);
        complete_state_loss_replacement(&mut receipt, &receipt_path, || {
            start_or_return_live_unlocked(addr)
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StateLossRecoveryPhase {
    Prepared,
    Reconciled,
    ReplacementStarted,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StateLossRecoveryReceipt {
    lease_id: String,
    recorded_pid: u32,
    recorded_endpoint: String,
    affected_job_ids: Vec<uuid::Uuid>,
    evidence_snapshot_path: String,
    ownership_proof: Vec<String>,
    phase: StateLossRecoveryPhase,
    replacement: Option<super::DaemonStartResult>,
}

impl StateLossRecoveryReceipt {
    fn into_result(self) -> Result<super::DaemonStateLossRecoveryResult> {
        let replacement = self.replacement.ok_or_else(|| {
            Error::internal_unexpected("state-loss receipt has no replacement daemon identity")
        })?;
        Ok(super::DaemonStateLossRecoveryResult { recovered_lease_id: self.lease_id, recorded_dead_pid: self.recorded_pid, recorded_endpoint: self.recorded_endpoint, affected_job_count: self.affected_job_ids.len(), affected_job_ids: self.affected_job_ids, evidence_snapshot_path: self.evidence_snapshot_path, ownership_proof: self.ownership_proof, retry_guidance: "Recorded outcomes were retained. Retry unfinished eligible work through its original command or workflow.".to_string(), replacement })
    }
}

fn validate_state_loss_preconditions(
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: SocketAddr,
    status: &super::DaemonStatus,
    receipt: Option<&StateLossRecoveryReceipt>,
) -> Result<String> {
    if status.state.is_some()
        || status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::LeaseMissing)
        || status.reachable
        || (receipt.is_none() && status.freshness.active_jobs == 0)
    {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "state-loss recovery requires an absent daemon state, unreachable endpoint, and active jobs or an exact recovery receipt",
            Some(lease_id.to_string()),
            None,
        ));
    }
    if pid_is_running(recorded_pid) {
        return Err(Error::validation_invalid_argument(
            "recorded_pid",
            format!("recorded daemon PID `{recorded_pid}` is still running"),
            Some(recorded_pid.to_string()),
            None,
        ));
    }
    probe_recorded_daemon_endpoint(recorded_endpoint)
}

fn validate_state_loss_receipt(
    receipt: &StateLossRecoveryReceipt,
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: SocketAddr,
) -> Result<()> {
    if receipt.lease_id != lease_id
        || receipt.recorded_pid != recorded_pid
        || receipt.recorded_endpoint != recorded_endpoint.to_string()
    {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "state-loss recovery receipt does not match the exact supplied lease, PID, and endpoint",
            Some(lease_id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn read_state_loss_receipt(path: &Path) -> Result<Option<StateLossRecoveryReceipt>> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw).map(Some).map_err(|error| {
            Error::internal_json(error.to_string(), Some(format!("read {}", path.display())))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!("read {}", path.display())),
        )),
    }
}

fn write_state_loss_receipt(path: &Path, receipt: &StateLossRecoveryReceipt) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal_unexpected("state-loss receipt path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let body = serde_json::to_vec_pretty(receipt).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize state-loss receipt".to_string()),
        )
    })?;
    std::fs::write(&temporary, body).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", path.display())),
        )
    })
}

fn state_loss_replacement_error(
    mut error: Error,
    receipt: &StateLossRecoveryReceipt,
    receipt_path: &Path,
) -> Error {
    error.details["state_loss_recovery"] = serde_json::json!({
        "receipt_path": receipt_path,
        "phase": receipt.phase,
        "lease_id": receipt.lease_id,
        "recorded_pid": receipt.recorded_pid,
        "recorded_endpoint": receipt.recorded_endpoint,
        "affected_job_ids": receipt.affected_job_ids,
        "evidence_snapshot_path": receipt.evidence_snapshot_path,
        "ownership_proof": receipt.ownership_proof,
    });
    error
}

fn complete_state_loss_replacement<Start>(
    receipt: &mut StateLossRecoveryReceipt,
    receipt_path: &Path,
    start: Start,
) -> Result<super::DaemonStateLossRecoveryResult>
where
    Start: FnOnce() -> Result<super::DaemonStartResult>,
{
    match start() {
        Ok(replacement) => {
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(replacement);
            write_state_loss_receipt(receipt_path, receipt)?;
            receipt.clone().into_result()
        }
        Err(error) => Err(state_loss_replacement_error(error, receipt, receipt_path)),
    }
}

fn recover_missing_lease_state_with_operations<
    Status,
    PidIsRunning,
    ProbeEndpoint,
    OwnerLock,
    AcquireOwner,
    Reconcile,
    Start,
>(
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: SocketAddr,
    status: Status,
    pid_is_running: PidIsRunning,
    probe_endpoint: ProbeEndpoint,
    acquire_owner: AcquireOwner,
    reconcile: Reconcile,
    start: Start,
) -> Result<super::DaemonStateLossRecoveryResult>
where
    Status: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: FnOnce(u32) -> bool,
    ProbeEndpoint: FnOnce(SocketAddr) -> Result<String>,
    AcquireOwner: FnOnce() -> Result<Option<OwnerLock>>,
    Reconcile: FnOnce() -> Result<(PathBuf, crate::core::api_jobs::DaemonLeaseJobDiagnostics)>,
    Start: FnOnce() -> Result<super::DaemonStartResult>,
{
    let status = status()?;
    if status.state.is_some()
        || status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::LeaseMissing)
        || status.freshness.active_jobs == 0
        || status.reachable
    {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "state-loss recovery requires an absent daemon state, unreachable endpoint, and active jobs",
            Some(lease_id.to_string()),
            None,
        ));
    }
    if pid_is_running(recorded_pid) {
        return Err(Error::validation_invalid_argument(
            "recorded_pid",
            format!("recorded daemon PID `{recorded_pid}` is still running"),
            Some(recorded_pid.to_string()),
            None,
        ));
    }
    let endpoint_probe = probe_endpoint(recorded_endpoint)?;
    let owner_lock = acquire_owner()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "daemon owner lock is held; refusing state-loss recovery",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    let (snapshot_path, reconciled) = reconcile()?;
    if reconciled.protected_count() > 0 {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "deferred missing-lease recovery because {} active child process(es) are still running: {}",
                reconciled.protected_count(),
                reconciled.protected_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            ),
            Some(lease_id.to_string()),
            Some(vec!["Wait for the recorded child process to finish, then retry recovery.".to_string()]),
        ));
    }
    if reconciled.matching_count() == 0 {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!("no active durable jobs belong to exact lease `{lease_id}`"),
            Some(lease_id.to_string()),
            None,
        ));
    }
    drop(owner_lock);
    let replacement = start()?;
    let affected_job_count = reconciled.matching_count();
    Ok(super::DaemonStateLossRecoveryResult {
        recovered_lease_id: lease_id.to_string(),
        recorded_dead_pid: recorded_pid,
        recorded_endpoint: recorded_endpoint.to_string(),
        affected_job_ids: reconciled.matching_job_ids,
        affected_job_count,
        evidence_snapshot_path: snapshot_path.display().to_string(),
        ownership_proof: vec![
            format!("operator supplied exact missing lease `{lease_id}`"),
            format!("recorded daemon PID `{recorded_pid}` was not running"),
            "daemon owner lock acquired non-destructively".to_string(),
            endpoint_probe,
        ],
        retry_guidance: "Recorded outcomes were retained. Retry unfinished eligible work through its original command or workflow.".to_string(),
        replacement,
    })
}

fn parse_recorded_daemon_endpoint(value: &str) -> Result<SocketAddr> {
    let endpoint = value.parse::<SocketAddr>().map_err(|_| {
        Error::validation_invalid_argument(
            "recorded_endpoint",
            "state-loss recovery requires a concrete recorded loopback endpoint",
            Some(value.to_string()),
            None,
        )
    })?;
    if endpoint.port() == 0 || endpoint.ip().is_unspecified() || !endpoint.ip().is_loopback() {
        return Err(Error::validation_invalid_argument(
            "recorded_endpoint",
            "recorded daemon endpoint must be a non-zero loopback address",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(endpoint)
}

fn probe_recorded_daemon_endpoint(endpoint: SocketAddr) -> Result<String> {
    match TcpStream::connect_timeout(&endpoint, Duration::from_millis(200)) {
        Ok(_) => Err(Error::validation_invalid_argument(
            "recorded_endpoint",
            format!("recorded daemon endpoint `{endpoint}` is reachable"),
            Some(endpoint.to_string()),
            None,
        )),
        Err(error) => Ok(format!(
            "recorded daemon endpoint `{endpoint}` was unreachable: {error}"
        )),
    }
}

fn reconcile_leaseless_orphan_store_with_operations<Status, Probe, Reconcile, Start>(
    status: Status,
    probe: Probe,
    reconcile: Reconcile,
    start: Start,
) -> Result<DaemonLeaselessOrphanReconciliationResult>
where
    Status: FnOnce() -> Result<super::DaemonStatus>,
    Probe: FnOnce() -> Result<Vec<String>>,
    Reconcile: FnOnce() -> Result<(
        PathBuf,
        crate::core::api_jobs::LeaselessOrphanJobDiagnostics,
    )>,
    Start: FnOnce() -> Result<super::DaemonStartResult>,
{
    let status = status()?;
    if status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::LeaseMissing)
        || status.freshness.active_jobs == 0
    {
        return Err(Error::validation_invalid_argument(
            "job_store",
            "lease-less reconciliation requires lease_missing with active jobs",
            None,
            None,
        ));
    }
    let no_owner_proof = probe()?;
    let (snapshot_path, reconciled) = reconcile()?;
    if !reconciled.protected_job_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "job_store",
            format!(
                "deferred lease-less recovery because {} active child process(es) are still running: {}",
                reconciled.protected_job_ids.len(),
                reconciled.protected_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            ),
            None,
            Some(vec!["Wait for the recorded child process to finish, then retry recovery.".to_string()]),
        ));
    }
    let affected_job_count = reconciled.reconciled_count();
    let replacement = start()?;
    Ok(DaemonLeaselessOrphanReconciliationResult {
        snapshot_path: snapshot_path.display().to_string(),
        affected_job_ids: reconciled.reconciled_job_ids.into_iter().map(|id| id.to_string()).collect(),
        affected_job_count,
        no_owner_proof,
        retry_guidance: "Inspect retained job events, then retry eligible work through its original command or workflow.".to_string(),
        replacement,
    })
}

/// Spawn the daemon in the background, then poll the state file until the new
/// process publishes its address (or a timeout elapses).
pub fn start_background(addr: &str) -> Result<DaemonStartResult> {
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    start_or_return_live_unlocked(addr)
}

/// Return a live daemon under the lifecycle lock, or start one when its lease
/// is absent or its recorded PID is dead.
pub fn ensure_running(addr: &str) -> Result<DaemonStartResult> {
    ensure_running_with_wait(addr, Duration::from_secs(5))
}

/// Replace one explicitly identified, provably dead daemon lease. The operation
/// lock covers validation, durable-job reconciliation, and replacement startup.
pub fn adopt_orphaned_lease(
    lease_id: &str,
    confirm_pid_dead: bool,
    addr: &str,
) -> Result<DaemonOrphanAdoptionResult> {
    if !confirm_pid_dead {
        return Err(Error::validation_invalid_argument(
            "confirm_pid_dead",
            "orphan adoption requires explicit confirmation that the recorded daemon PID is dead",
            None,
            Some(vec!["Inspect `homeboy daemon status` and retry with --confirm-pid-dead only for the reported dead lease.".to_string()]),
        ));
    }
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    let status = read_status()?;
    let state = status.state.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "orphan adoption requires a persisted daemon lease",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    if state.lease_id != lease_id {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "recorded daemon lease `{}` does not match requested orphan lease `{lease_id}`",
                state.lease_id
            ),
            Some(lease_id.to_string()),
            Some(vec![
                "Run `homeboy daemon status` and adopt only its exact dead lease.".to_string(),
            ]),
        ));
    }
    if status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::PidDead) {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!("daemon lease `{lease_id}` is not proven dead"),
            Some(lease_id.to_string()),
            Some(vec!["Live or ambiguous daemon ownership is protected; inspect `homeboy daemon status` before retrying.".to_string()]),
        ));
    }

    let owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "daemon owner lock is held; refusing exact dead-lease adoption",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    let store =
        super::JobStore::open_without_reconciliation(crate::core::paths::daemon_jobs_file()?)?;
    let reconciled = store.reconcile_dead_daemon_lease_jobs(lease_id)?;
    if reconciled.protected_count() > 0 {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "deferred exact dead-lease adoption because {} active child process(es) are still running: {}",
                reconciled.protected_count(),
                reconciled.protected_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            ),
            Some(lease_id.to_string()),
            Some(vec!["Wait for the recorded child process to finish, then retry adoption.".to_string()]),
        ));
    }
    drop(owner_lock);
    let replacement = start_or_return_live_unlocked(addr)?;
    Ok(DaemonOrphanAdoptionResult {
        adopted_lease_id: lease_id.to_string(),
        dead_pid: state.pid,
        active_jobs_terminalized: reconciled.terminalized_count(),
        retry_guidance: "Inspect the retained job events, then retry eligible work through its original command or workflow.".to_string(),
        replacement,
    })
}

/// Explicitly recover legacy unowned durable jobs when the daemon lease is
/// missing. Process and configured-listener probes are fail-closed because no
/// lease identity exists to adopt.
pub fn reconcile_leaseless_orphans(
    confirm_no_daemon_owner: bool,
    addr: &str,
) -> Result<DaemonLeaselessRecoveryResult> {
    if !confirm_no_daemon_owner {
        return Err(Error::validation_invalid_argument(
            "confirm_no_daemon_owner",
            "lease-less recovery requires --confirm-no-daemon-owner",
            None,
            None,
        ));
    }
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    let status = read_status()?;
    if status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::LeaseMissing) {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "lease-less recovery requires a missing daemon lease; use exact lease recovery for recorded leases",
            None,
            None,
        ));
    }
    let owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "daemon owner lock is held; a daemon is live or starting",
            None,
            None,
        )
    })?;
    let ownership_proof = prove_no_daemon_owner(addr)?;
    let jobs_path = crate::core::paths::daemon_jobs_file()?;
    let raw_store = read_job_store_bytes(&jobs_path)?;
    let snapshot_path = snapshot_job_store(&jobs_path, &raw_store)?;
    let store = super::JobStore::open_without_reconciliation_from_bytes(&jobs_path, &raw_store)?;
    let reconciled = store.reconcile_leaseless_orphan_jobs()?;
    if !reconciled.protected_job_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            format!(
                "deferred lease-less recovery because {} active child process(es) are still running: {}",
                reconciled.protected_job_ids.len(),
                reconciled.protected_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            ),
            None,
            Some(vec!["Wait for the recorded child process to finish, then retry recovery.".to_string()]),
        ));
    }
    let affected_job_count = reconciled.reconciled_count();
    drop(owner_lock);
    let replacement = start_or_return_live_unlocked(addr)?;
    Ok(DaemonLeaselessRecoveryResult {
        affected_job_ids: reconciled.reconciled_job_ids,
        affected_job_count,
        affected_jobs: reconciled.affected_jobs,
        historical_lease_ids: reconciled.historical_lease_ids,
        evidence_snapshot_path: snapshot_path.display().to_string(),
        ownership_proof,
        retry_guidance: "Recorded job output and artifacts were retained. Retry eligible work through its original command or workflow.".to_string(),
        replacement,
    })
}

fn prove_no_daemon_owner(addr: &str) -> Result<Vec<String>> {
    // The owner lock proves no serving daemon owns this store. Refuse any
    // matching process or configured listener as additional ambiguous ownership.
    let output = Command::new("ps")
        .args(["-ax", "-o", "command="])
        .output()
        .ok();
    let parsed: SocketAddr = addr.parse().map_err(|_| {
        Error::validation_invalid_argument("addr", "invalid daemon address", None, None)
    })?;
    let process = match output {
        Some(output) if output.status.success() => {
            let matched = String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.contains("homeboy daemon serve"));
            if matched {
                return Err(Error::validation_invalid_argument(
                    "owner_probe",
                    "a Homeboy daemon serve process is present; refusing ambiguous missing-lease recovery",
                    None,
                    None,
                ));
            } else {
                "supplemental process probe found no matching command"
            }
        }
        _ => {
            return Err(Error::validation_invalid_argument(
                "owner_probe",
                "unable to inspect Homeboy daemon processes; refusing ambiguous missing-lease recovery",
                None,
                None,
            ));
        }
    };
    let listener = if parsed.port() == 0 {
        format!("listener probe has no fixed address for dynamic bind {addr}")
    } else if TcpStream::connect_timeout(&parsed, Duration::from_millis(200)).is_ok() {
        return Err(Error::validation_invalid_argument(
            "owner_probe",
            format!("a daemon listener is reachable at {addr}; refusing missing-lease recovery"),
            None,
            None,
        ));
    } else {
        format!("supplemental listener probe found no listener at {addr}")
    };
    Ok(vec![
        "daemon owner lock acquired non-destructively".to_string(),
        process.to_string(),
        listener,
    ])
}

fn read_job_store_bytes(path: &Path) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(raw) => Ok(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(b"{\"jobs\":[]}".to_vec()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!("read {}", path.display())),
        )),
    }
}

fn snapshot_job_store(path: &Path, raw: &[u8]) -> Result<PathBuf> {
    let snapshot = path.with_file_name(format!(
        "{}.leaseless-orphan-{}.snapshot",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("jobs.json"),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&snapshot, raw).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("snapshot {}", path.display())),
        )
    })?;
    Ok(snapshot)
}

fn reconcile_dead_daemon_lease_jobs(expected_lease_id: &str) -> Result<()> {
    let store =
        super::JobStore::open_without_reconciliation(crate::core::paths::daemon_jobs_file()?)?;
    store.reconcile_dead_daemon_lease_jobs(expected_lease_id)?;
    Ok(())
}

fn ensure_running_with_wait(addr: &str, wait: Duration) -> Result<DaemonStartResult> {
    parse_bind_addr(addr)?;
    ensure_running_with_operations(
        wait,
        acquire_daemon_operation_lock_for_ensure,
        read_status,
        pid_is_running,
        || start_or_return_live_unlocked(addr),
    )
}

fn reconcile_dead_lease_and_ensure_running_with_operations<
    Lock,
    AcquireLock,
    ReadStatus,
    PidIsRunning,
    Reconcile,
    Start,
>(
    wait: Duration,
    acquire_lock: AcquireLock,
    expected_lease_id: &str,
    read_status: ReadStatus,
    pid_is_running: PidIsRunning,
    reconcile: Reconcile,
    start: Start,
) -> Result<DaemonStartResult>
where
    AcquireLock: FnOnce(Duration) -> Result<Lock>,
    ReadStatus: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: FnOnce(u32) -> bool,
    Reconcile: FnOnce() -> Result<()>,
    Start: FnOnce() -> Result<DaemonStartResult>,
{
    let _lock = acquire_lock(wait)?;
    let status = read_status()?;
    let state = status.state.ok_or_else(|| {
        Error::validation_invalid_argument(
            "expected-lease-id",
            "remote daemon has no recorded lease; refusing dead-lease reconciliation",
            Some(expected_lease_id.to_string()),
            None,
        )
    })?;
    if pid_is_running(state.pid) {
        return Ok(DaemonStartResult {
            pid: state.pid,
            address: state.address,
            state_path: state.state_path,
            lease_id: state.lease_id,
        });
    }
    if status.freshness.stale_reason_code != Some(super::DaemonStaleReasonCode::PidDead) {
        return Err(Error::validation_invalid_argument(
            "expected-lease-id",
            "remote daemon PID is not proven dead; refusing dead-lease reconciliation",
            Some(expected_lease_id.to_string()),
            None,
        ));
    }
    if state.lease_id != expected_lease_id {
        return Err(Error::validation_invalid_argument(
            "expected-lease-id",
            format!(
                "remote daemon lease `{}` does not match expected stale lease; refusing reconciliation",
                state.lease_id
            ),
            Some(expected_lease_id.to_string()),
            None,
        ));
    }

    reconcile()?;
    start()
}

fn ensure_running_with_operations<Lock, AcquireLock, ReadStatus, PidIsRunning, Start>(
    wait: Duration,
    acquire_lock: AcquireLock,
    read_status: ReadStatus,
    pid_is_running: PidIsRunning,
    start: Start,
) -> Result<DaemonStartResult>
where
    AcquireLock: FnOnce(Duration) -> Result<Lock>,
    ReadStatus: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: FnOnce(u32) -> bool,
    Start: FnOnce() -> Result<DaemonStartResult>,
{
    let _lock = acquire_lock(wait)?;
    let status = read_status()?;
    if let Some(state) = status.state {
        if pid_is_running(state.pid) {
            return Ok(DaemonStartResult {
                pid: state.pid,
                address: state.address,
                state_path: state.state_path,
                lease_id: state.lease_id,
            });
        }
    }
    start()
}

/// Called only while the controller lifecycle lock is held. `serve` deliberately
/// does not take that lock: it uses the owner lock, allowing this parent to wait
/// for lease publication without a parent/child startup deadlock.
fn start_or_return_live_unlocked(addr: &str) -> Result<DaemonStartResult> {
    let _repaired_legacy_lease = repair_legacy_lease_for_start()?;
    start_or_return_live_with_operations(
        read_status,
        try_acquire_daemon_owner_lock,
        || stop_unlocked().map(|_| ()),
        || spawn_and_wait_for_lease(addr),
    )
}

fn start_or_return_live_with_operations<OwnerLock, ReadStatus, AcquireOwner, Cleanup, Spawn>(
    read_status: ReadStatus,
    acquire_owner: AcquireOwner,
    cleanup: Cleanup,
    spawn_and_wait: Spawn,
) -> Result<DaemonStartResult>
where
    ReadStatus: FnOnce() -> Result<super::DaemonStatus>,
    AcquireOwner: FnOnce() -> Result<Option<OwnerLock>>,
    Cleanup: FnOnce() -> Result<()>,
    Spawn: FnOnce() -> Result<DaemonStartResult>,
{
    let existing = read_status()?;
    if existing.running {
        if let Some(state) = existing.state {
            return Ok(DaemonStartResult {
                pid: state.pid,
                address: state.address,
                state_path: state.state_path,
                lease_id: state.lease_id,
            });
        }
    }
    if existing.state.is_some() || existing.stale_reason.is_some() {
        let owner_lock = acquire_owner()?.ok_or_else(|| {
            Error::internal_unexpected(
                "daemon owner is live or starting; refusing stale lease cleanup",
            )
        })?;
        cleanup()?;
        drop(owner_lock);
    }

    spawn_and_wait()
}

fn spawn_and_wait_for_lease(addr: &str) -> Result<DaemonStartResult> {
    let exe = std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let startup_token = uuid::Uuid::new_v4().to_string();
    let child = Command::new(exe)
        .args(["daemon", "serve", "--addr", addr])
        .env(DAEMON_STARTUP_TOKEN_ENV, &startup_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::internal_io(e.to_string(), Some("spawn daemon".to_string())))?;
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = read_status()?;
        if let Some(state) = status.state {
            if state.pid == pid && state.startup_token == startup_token {
                return Ok(DaemonStartResult {
                    pid,
                    address: state.address,
                    state_path: state.state_path,
                    lease_id: state.lease_id,
                });
            }
            if status.running {
                return Ok(DaemonStartResult {
                    pid: state.pid,
                    address: state.address,
                    state_path: state.state_path,
                    lease_id: state.lease_id,
                });
            }
        }

        if Instant::now() >= deadline {
            return Err(Error::internal_unexpected(format!(
                "daemon process {} did not publish matching startup token before timeout",
                pid
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

/// Resolve the daemon base URL, falling back to the running daemon's address.
fn resolve_daemon_url(daemon_url: Option<String>) -> Result<String> {
    if let Some(url) = daemon_url.filter(|url| !url.trim().is_empty()) {
        return Ok(url);
    }
    let status = read_status()?;
    let Some(state) = status.state.filter(|_| status.running) else {
        return Err(Error::validation_invalid_argument(
            "daemon-url",
            "daemon is not running; pass --daemon-url or start it with `homeboy daemon start`",
            None,
            None,
        ));
    };
    Ok(format!("http://{}", state.address))
}

/// Build the encoded daemon byte-endpoint URL for a given run/artifact pair.
pub fn artifact_content_url(daemon_url: &str, run_id: &str, artifact_id: &str) -> Result<String> {
    let mut base = reqwest::Url::parse(daemon_url).map_err(|e| {
        Error::validation_invalid_argument(
            "daemon-url",
            e.to_string(),
            Some(daemon_url.to_string()),
            None,
        )
    })?;
    base.set_path(&format!(
        "/runs/{}/artifacts/{}/content",
        encode_uri_component(run_id),
        encode_uri_component(artifact_id)
    ));
    base.set_query(None);
    Ok(base.to_string())
}

/// Fetch artifact bytes through the local daemon byte endpoint and persist them.
///
/// Resolves the daemon URL, downloads the content, ensures the parent directory
/// exists, and writes the bytes to `output`. Returns metadata describing the
/// download for the caller to render.
pub fn fetch_artifact_to_path(
    run_id: &str,
    artifact_id: &str,
    daemon_url: Option<String>,
    output: Option<PathBuf>,
) -> Result<ArtifactFetchOutcome> {
    let daemon_url = resolve_daemon_url(daemon_url)?;
    let content_url = artifact_content_url(&daemon_url, run_id, artifact_id)?;
    let output_path = output.unwrap_or_else(|| default_artifact_output_path(artifact_id));

    let response = reqwest::blocking::get(&content_url).map_err(reqwest_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "daemon artifact fetch failed with HTTP {}: {}",
                status.as_u16(),
                body
            ),
            Some(artifact_id.to_string()),
            None,
        ));
    }

    let bytes = response.bytes().map_err(reqwest_error)?;
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }
    std::fs::write(&output_path, &bytes).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write {}", output_path.display())),
        )
    })?;

    Ok(ArtifactFetchOutcome {
        daemon_url,
        content_url,
        output_path,
        content_type: header_value(&headers, reqwest::header::CONTENT_TYPE.as_str()),
        size_bytes: bytes.len() as u64,
        sha256: header_value(&headers, "x-homeboy-artifact-sha256"),
    })
}

fn default_artifact_output_path(artifact_id: &str) -> PathBuf {
    artifact_id
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("artifact.bin"))
}

fn header_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn reqwest_error(error: reqwest::Error) -> Error {
    Error::internal_io(error.to_string(), Some("fetch daemon artifact".to_string()))
}

#[cfg(test)]
#[path = "../../../tests/core/daemon/control_test.rs"]
mod control_test;
