//! Local daemon lifecycle and artifact-fetch orchestration owned by core.
//!
//! The command layer (`src/commands/daemon.rs`) stays a thin adapter: it parses
//! arguments and renders output. The process spawning, status polling, HTTP
//! artifact fetch, and filesystem persistence live here so the orchestration is
//! testable and reusable outside the CLI.

use std::collections::VecDeque;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::execution_contract::encode_uri_component;
use crate::process::{
    pid_has_ownership_token, pid_is_running, signal_pid, wait_for_pid_exit, SIGNAL_KILL,
    SIGNAL_TERMINATE,
};
use serde::{Deserialize, Serialize};

use super::acquire_daemon_job_admission_fence;
use super::{
    acquire_daemon_operation_lock, acquire_daemon_operation_lock_for_ensure, parse_bind_addr,
    read_status, stop_unlocked, try_acquire_daemon_owner_lock, DaemonExactOrphanRecoveryResult,
    DaemonLeaselessRecoveryResult, DaemonOrphanAdoptionResult, DaemonStaleReasonCode,
    DaemonStartResult, DaemonTerminationClassification, DAEMON_STARTUP_TOKEN_ENV,
};

const TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV: &str = "HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP";
const TEST_KEEP_DAEMON_IN_LAUNCHER_GROUP: &str = "launcher";
// A launcher can observe two five-second lease-publication attempts and clean
// up the first attempt before returning. Followers must wait through that
// bounded startup lifecycle, then re-read the durable lease rather than timing
// out halfway through the winning launch.
const ENSURE_RUNNING_STARTUP_WAIT: Duration = Duration::from_secs(15);

#[cfg(all(test, unix))]
#[test]
fn failed_startup_guard_reaps_its_exact_launcher_even_with_a_corrupt_lease() {
    crate::test_support::with_isolated_home(|_| {
        for explicit_error in [false, true] {
            let token = uuid::Uuid::new_v4().to_string();
            let state_path = crate::paths::daemon_state_file().unwrap();
            std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
            std::fs::write(&state_path, b"corrupt startup lease").unwrap();
            let child = Command::new("sh")
                .args([
                    "-c",
                    "trap 'exit 0' TERM; while :; do sleep 0.1; done",
                    "homeboy",
                    "--startup-token",
                    &token,
                ])
                .env(DAEMON_STARTUP_TOKEN_ENV, &token)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id();
            let mut attempt = StartupAttempt::new(child, &token, &state_path);
            if explicit_error {
                let error = attempt
                    .finish(Err(Error::internal_unexpected("original startup failure")))
                    .unwrap_err();
                assert!(error.to_string().contains("original startup failure"));
                assert!(error.details["startup_cleanup"].is_array());
            }
            drop(attempt);
            assert!(
                !pid_is_running(pid),
                "unpublished owned launcher must be reaped"
            );
            assert_eq!(
                std::fs::read(&state_path).unwrap(),
                b"corrupt startup lease"
            );
        }
    });
}

/// Redact complete, bounded records before retaining them. An overlong record
/// is discarded rather than retaining an unredacted suffix whose key occurred
/// before the retention boundary.
fn bounded_redacted_reader(mut reader: impl Read) -> Option<String> {
    const LIMIT: usize = 4096;
    const RECORD_LIMIT: usize = 4096;
    let mut tail = VecDeque::with_capacity(LIMIT);
    let mut buffer = [0; 8192];
    let mut record = Vec::with_capacity(RECORD_LIMIT);
    let mut discarding_record = false;
    let mut truncated = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) => {
                append_redacted_tail(
                    &mut tail,
                    LIMIT,
                    &format!("[output read failed: {:?}]", error.kind()),
                );
                truncated = true;
                break;
            }
        };
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if discarding_record {
                    discarding_record = false;
                    truncated = true;
                } else {
                    append_redacted_tail(&mut tail, LIMIT, &String::from_utf8_lossy(&record));
                }
                record.clear();
            } else if !discarding_record {
                if record.len() == RECORD_LIMIT {
                    // The record may contain a secret that spans its boundary.
                    // Keep none of it until a newline starts a new record.
                    record.clear();
                    discarding_record = true;
                    truncated = true;
                } else {
                    record.push(*byte);
                }
            }
        }
    }
    if !record.is_empty() && !discarding_record {
        append_redacted_tail(&mut tail, LIMIT, &String::from_utf8_lossy(&record));
    }
    if tail.is_empty() {
        return None;
    }
    let bytes = tail.make_contiguous();
    let mut text = String::from_utf8_lossy(bytes).to_string();
    if truncated {
        text.push_str("\n[truncated]");
    }
    Some(text)
}

fn append_redacted_tail(tail: &mut VecDeque<u8>, limit: usize, record: &str) {
    let redacted = crate::redaction::redact_string(record);
    let bytes = redacted.as_bytes();
    if bytes.len() >= limit {
        tail.clear();
        tail.extend(&bytes[bytes.len() - limit..]);
        return;
    }
    let overflow = tail.len().saturating_add(bytes.len()).saturating_sub(limit);
    if overflow > 0 {
        tail.drain(..overflow);
    }
    tail.extend(bytes);
}

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
///
/// PID death and control-plane loss are not operator assertions: both are
/// established here. `validate_state_loss_preconditions` requires an absent
/// state record, a `LeaseMissing` freshness code, an unreachable daemon, and a
/// non-running recorded PID, and then `probe_recorded_daemon_endpoint` must
/// fail to connect to the recorded endpoint.
pub fn recover_missing_lease_state(
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: &str,
    addr: &str,
    replacement_operation_id: Option<&str>,
) -> Result<super::DaemonStateLossRecoveryResult> {
    parse_bind_addr(addr)?;
    let recorded_endpoint = parse_recorded_daemon_endpoint(recorded_endpoint)?;
    let _lock = acquire_daemon_operation_lock()?;
    let receipt_path = crate::paths::daemon_state_loss_recovery_receipt_file(lease_id)?;
    let status = read_status()?;
    let mut existing = read_state_loss_receipt(&receipt_path)?;
    if let Some(receipt) = existing.as_mut() {
        validate_state_loss_receipt(
            receipt,
            lease_id,
            recorded_pid,
            recorded_endpoint,
            replacement_operation_id,
        )?;
        // Receipts created before operation IDs were introduced are safe to
        // adopt only after the immutable recovery inputs above matched.
        if receipt.replacement_operation_id.is_none() {
            if let Some(operation_id) = replacement_operation_id {
                receipt.replacement_operation_id = Some(operation_id.to_string());
                write_state_loss_receipt(&receipt_path, receipt)?;
            }
        }
        if receipt.phase == StateLossRecoveryPhase::ReplacementStarted {
            return receipt.clone().into_result();
        }
        if receipt.phase == StateLossRecoveryPhase::ReplacementStarting {
            if pid_is_running(recorded_pid) {
                return Err(Error::validation_invalid_argument(
                    "recorded_pid",
                    format!("recorded daemon PID `{recorded_pid}` is still running"),
                    Some(recorded_pid.to_string()),
                    None,
                ));
            }
            probe_recorded_daemon_endpoint(recorded_endpoint)?;
            return replay_replacement_starting(receipt.clone(), &receipt_path, &status, addr);
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
        start_state_loss_replacement(&mut receipt, &receipt_path, addr)
    } else {
        let owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "lease_id",
                "daemon owner lock is held; refusing state-loss recovery",
                Some(lease_id.to_string()),
                None,
            )
        })?;
        let jobs_path = crate::paths::observation_db()?;
        let snapshot_path = snapshot_job_store(&jobs_path)?;
        let store = super::JobStore::open_without_reconciliation(&jobs_path)?;
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
            replacement_startup_token: None,
            replacement_operation_id: replacement_operation_id.map(str::to_string),
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
        start_state_loss_replacement(&mut receipt, &receipt_path, addr)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StateLossRecoveryPhase {
    Prepared,
    Reconciled,
    ReplacementStarting,
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
    #[serde(default)]
    replacement_startup_token: Option<String>,
    #[serde(default)]
    replacement_operation_id: Option<String>,
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
    replacement_operation_id: Option<&str>,
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
    if let Some(operation_id) = replacement_operation_id {
        if receipt.replacement_operation_id.is_some()
            && receipt.replacement_operation_id.as_deref() != Some(operation_id)
        {
            return Err(Error::validation_invalid_argument(
                "replacement_operation_id",
                "state-loss recovery receipt belongs to a different replacement operation",
                Some(operation_id.to_string()),
                None,
            ));
        }
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
    let mut file = std::fs::File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", temporary.display())),
        )
    })?;
    use std::io::Write;
    file.write_all(&body)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
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
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
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

fn start_state_loss_replacement(
    receipt: &mut StateLossRecoveryReceipt,
    receipt_path: &Path,
    addr: &str,
) -> Result<super::DaemonStateLossRecoveryResult> {
    start_state_loss_replacement_with(receipt, receipt_path, |startup_token| {
        let replacement = start_or_return_live_unlocked_with_startup_token(addr, startup_token)?;
        let status = read_status()?;
        if status.admits_work()
            && status
                .state
                .as_ref()
                .is_some_and(|state| state.startup_token == startup_token)
        {
            Ok(replacement)
        } else {
            Err(Error::validation_invalid_argument(
                "lease_id",
                "replacement startup did not publish the expected state-loss startup token",
                None,
                None,
            ))
        }
    })
}

fn start_state_loss_replacement_with<Start>(
    receipt: &mut StateLossRecoveryReceipt,
    receipt_path: &Path,
    start: Start,
) -> Result<super::DaemonStateLossRecoveryResult>
where
    Start: FnOnce(&str) -> Result<super::DaemonStartResult>,
{
    let startup_token = receipt
        .replacement_startup_token
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    receipt.phase = StateLossRecoveryPhase::ReplacementStarting;
    receipt.replacement_startup_token = Some(startup_token.clone());
    write_state_loss_receipt(receipt_path, receipt)?;
    complete_state_loss_replacement(receipt, receipt_path, || start(&startup_token))
}

fn replay_replacement_starting(
    mut receipt: StateLossRecoveryReceipt,
    receipt_path: &Path,
    status: &super::DaemonStatus,
    addr: &str,
) -> Result<super::DaemonStateLossRecoveryResult> {
    let token = receipt
        .replacement_startup_token
        .as_deref()
        .ok_or_else(|| {
            Error::internal_unexpected("replacement-starting receipt has no startup token")
        })?;
    if let Some(state) = status.state.as_ref() {
        if status.admits_work() && state.startup_token == token {
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(super::DaemonStartResult {
                pid: state.pid,
                address: state.address.clone(),
                state_path: state.state_path.clone(),
                lease_id: state.lease_id.clone(),
            });
            write_state_loss_receipt(receipt_path, &receipt)?;
            return receipt.into_result();
        }
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "state-loss replay found an ambiguous or mismatched live daemon",
            Some(receipt.lease_id),
            None,
        ));
    }
    start_state_loss_replacement(&mut receipt, receipt_path, addr)
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
            let previous_phase = receipt.phase.clone();
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(replacement);
            if let Err(error) = write_state_loss_receipt(receipt_path, receipt) {
                receipt.phase = previous_phase;
                receipt.replacement = None;
                return Err(state_loss_replacement_error(error, receipt, receipt_path));
            }
            receipt.clone().into_result()
        }
        Err(error) => Err(state_loss_replacement_error(error, receipt, receipt_path)),
    }
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LeaselessRecoveryReceipt {
    affected_job_ids: Vec<uuid::Uuid>,
    affected_jobs: Vec<crate::api_jobs::LeaselessOrphanAffectedJob>,
    historical_lease_ids: Vec<String>,
    evidence_snapshot_path: String,
    ownership_proof: Vec<String>,
    phase: StateLossRecoveryPhase,
    replacement: Option<DaemonStartResult>,
    replacement_startup_token: Option<String>,
    #[serde(default)]
    replacement_operation_id: Option<String>,
}

impl LeaselessRecoveryReceipt {
    fn into_result(self) -> Result<DaemonLeaselessRecoveryResult> {
        let replacement = self.replacement.ok_or_else(|| {
            Error::internal_unexpected("lease-less receipt has no replacement daemon identity")
        })?;
        Ok(DaemonLeaselessRecoveryResult {
            affected_job_count: self.affected_job_ids.len(),
            affected_job_ids: self.affected_job_ids,
            affected_jobs: self.affected_jobs,
            historical_lease_ids: self.historical_lease_ids,
            evidence_snapshot_path: self.evidence_snapshot_path,
            ownership_proof: self.ownership_proof,
            retry_guidance: "Recovery already completed for this exact replacement daemon; no additional daemon was started.".to_string(),
            replacement,
        })
    }
}

fn replay_leaseless_recovery(
    status: &super::DaemonStatus,
    addr: &str,
    replacement_operation_id: Option<&str>,
) -> Result<Option<DaemonLeaselessRecoveryResult>> {
    let receipt_path = crate::paths::daemon_leaseless_recovery_receipt_file()?;
    let Some(mut receipt) = read_leaseless_recovery_receipt(&receipt_path)? else {
        return Ok(None);
    };
    if let Some(operation_id) = replacement_operation_id {
        if receipt.replacement_operation_id.is_some()
            && receipt.replacement_operation_id.as_deref() != Some(operation_id)
        {
            return Err(Error::validation_invalid_argument(
                "replacement_operation_id",
                "lease-less recovery receipt belongs to a different replacement operation",
                Some(operation_id.to_string()),
                None,
            ));
        }
        if receipt.replacement_operation_id.is_none() {
            receipt.replacement_operation_id = Some(operation_id.to_string());
            write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
        }
    }
    if receipt.phase == StateLossRecoveryPhase::Prepared {
        if status.freshness.active_jobs > 0 {
            return Ok(None);
        }
        receipt.phase = StateLossRecoveryPhase::Reconciled;
        write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    }
    if receipt.phase == StateLossRecoveryPhase::Reconciled {
        receipt.phase = StateLossRecoveryPhase::ReplacementStarting;
        receipt.replacement_startup_token = Some(uuid::Uuid::new_v4().to_string());
        write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
        return replay_leaseless_recovery(status, addr, replacement_operation_id);
    }
    if receipt.phase == StateLossRecoveryPhase::ReplacementStarting {
        if let Some(state) = status.state.as_ref().filter(|_| status.admits_work()) {
            if receipt.replacement_startup_token.as_deref() != Some(&state.startup_token) {
                return Err(Error::validation_invalid_argument(
                    "reconcile_leaseless_orphans",
                    "lease-less recovery replay found a mismatched live daemon",
                    None,
                    None,
                ));
            }
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(DaemonStartResult {
                pid: state.pid,
                address: state.address.clone(),
                state_path: state.state_path.clone(),
                lease_id: state.lease_id.clone(),
            });
            write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
        } else {
            let token = receipt
                .replacement_startup_token
                .as_deref()
                .ok_or_else(|| {
                    Error::internal_unexpected(
                        "lease-less replacement-starting receipt has no startup token",
                    )
                })?;
            let replacement = start_or_return_live_unlocked_with_startup_token(addr, token)?;
            let started = read_status()?;
            if !started.admits_work()
                || started
                    .state
                    .as_ref()
                    .is_none_or(|state| state.startup_token != token)
            {
                return Err(Error::validation_invalid_argument(
                    "reconcile_leaseless_orphans",
                    "replacement replay did not publish its expected startup token",
                    None,
                    None,
                ));
            }
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(replacement);
            write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
            return Ok(Some(receipt.into_result()?));
        }
    }
    let Some(state) = status.state.as_ref() else {
        return Ok(None);
    };
    if status.freshness.active_jobs != 0 || !status.admits_work() {
        return Ok(None);
    }
    let replacement = receipt.replacement.as_ref().ok_or_else(|| {
        Error::internal_unexpected("completed lease-less receipt has no replacement")
    })?;
    if receipt.phase != StateLossRecoveryPhase::ReplacementStarted
        || replacement.lease_id != state.lease_id
        || replacement.pid != state.pid
        || replacement.address != state.address
    {
        return Ok(None);
    }
    Ok(Some(receipt.into_result()?))
}

fn read_leaseless_recovery_receipt(path: &Path) -> Result<Option<LeaselessRecoveryReceipt>> {
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

fn write_leaseless_recovery_receipt(path: &Path, receipt: &LeaselessRecoveryReceipt) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_unexpected("lease-less recovery receipt path has no parent")
    })?;
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
            Some("serialize lease-less recovery receipt".to_string()),
        )
    })?;
    let mut file = std::fs::File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", temporary.display())),
        )
    })?;
    use std::io::Write;
    file.write_all(&body)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
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
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })
}

/// Spawn the daemon in the background, then poll the state file until the new
/// process publishes its address (or a timeout elapses).
pub fn start_background(addr: &str) -> Result<DaemonStartResult> {
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    start_or_return_live_unlocked_with_idle_timeout(addr, &uuid::Uuid::new_v4().to_string(), 0)
}

/// Return a live daemon under the lifecycle lock, or start one when its lease
/// is absent or its recorded PID is dead. A live daemon on another build is
/// restarted in place: the stop interrupts its in-flight work and the
/// replacement resumes that work from the same store (#15936).
pub fn ensure_running(addr: &str) -> Result<DaemonStartResult> {
    let status = read_status()?;
    if let Some(state) = status
        .state
        .as_ref()
        .filter(|_| status.running && !status.fresh)
    {
        // A service manager starts the replacement itself; spawning one here
        // would race it for the owner lock (#17387).
        if state.service_manager.is_some() {
            return restart_service_managed(&state.lease_id, super::DaemonStatus::admits_work);
        }
        restart_in_place(&state.lease_id)?;
    }
    ensure_running_with_wait(addr, ENSURE_RUNNING_STARTUP_WAIT)
}

const SERVICE_MANAGED_RESTART_POLL: Duration = Duration::from_millis(250);
/// Covers the old daemon's drain (helpers join within 10s) plus the
/// manager's restart delay and the replacement's startup recovery.
const SERVICE_MANAGED_RESTART_POLLS: usize = 360;

/// Restart the live service-managed daemon `lease_id` without competing with
/// its manager: SIGTERM the exact leased process, which drains gracefully,
/// then wait for the manager to start the replacement and for the
/// replacement's status to satisfy `replaced` (for example, report the new
/// build). Never spawns a daemon (#17387).
pub fn restart_service_managed(
    lease_id: &str,
    replaced: impl Fn(&super::DaemonStatus) -> bool,
) -> Result<DaemonStartResult> {
    // Held throughout, so a concurrent `ensure-running` waits for the
    // replacement instead of spawning into the restart window.
    let _lock = acquire_daemon_operation_lock()?;
    restart_service_managed_with(
        lease_id,
        replaced,
        read_status,
        terminate_service_managed_daemon,
        || thread::sleep(SERVICE_MANAGED_RESTART_POLL),
        SERVICE_MANAGED_RESTART_POLLS,
    )
}

fn restart_service_managed_with<ReadStatus, Terminate, Sleep>(
    lease_id: &str,
    replaced: impl Fn(&super::DaemonStatus) -> bool,
    mut read_status: ReadStatus,
    terminate: Terminate,
    mut sleep: Sleep,
    polls: usize,
) -> Result<DaemonStartResult>
where
    ReadStatus: FnMut() -> Result<super::DaemonStatus>,
    Terminate: FnOnce(&super::DaemonState) -> Result<()>,
    Sleep: FnMut(),
{
    let status = read_status()?;
    let state = status
        .state
        .clone()
        .filter(|state| status.running && state.lease_id == lease_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "lease_id",
                format!("no live daemon holds lease `{lease_id}`; nothing to restart"),
                Some(lease_id.to_string()),
                None,
            )
        })?;
    let manager = state.service_manager.ok_or_else(|| {
        Error::internal_unexpected(format!(
            "daemon lease `{lease_id}` is not owned by a service manager"
        ))
    })?;
    terminate(&state)?;
    let mut last = status;
    for _ in 0..polls {
        sleep();
        last = read_status()?;
        if let Some(current) = last
            .state
            .as_ref()
            .filter(|current| last.running && current.lease_id != lease_id && replaced(&last))
        {
            return Ok(DaemonStartResult {
                pid: current.pid,
                address: current.address.clone(),
                state_path: current.state_path.clone(),
                lease_id: current.lease_id.clone(),
            });
        }
    }
    let observed = match last.state.as_ref() {
        Some(current) if current.lease_id == lease_id && last.running => {
            format!("pid {} is still serving lease `{lease_id}`", current.pid)
        }
        Some(current) if last.running => format!(
            "its replacement lease `{}` runs {}, which is not the expected build",
            current.lease_id, current.build_identity.display
        ),
        _ => "no replacement daemon lease was published".to_string(),
    };
    Err(Error::internal_unexpected(format!(
        "service manager {} did not bring up a replacement daemon: {observed}; Homeboy does not start one beside it",
        manager.as_str()
    ))
    .with_hint(format!(
        "Inspect the {} service for the daemon (it must restart on exit, e.g. `Restart=always`) and the binary it runs.",
        manager.as_str()
    )))
}

/// SIGTERM the exact service-managed daemon recorded by `state`. It carries
/// no Homeboy launcher and possibly no startup token, so the proof that the PID is
/// still that daemon is the unchanged lease plus the owner lock holder record
/// naming it.
fn terminate_service_managed_daemon(state: &super::DaemonState) -> Result<()> {
    let path = crate::paths::daemon_state_file()?;
    let current = super::read_lease_if_identity_matches(
        &path,
        &super::DaemonLeaseIdentity::from_state(state),
    )?;
    if current.pid != state.pid
        || !pid_is_running(state.pid)
        || !super::probe_daemon_owner_lock_at(&path)?
            .holder()
            .is_some_and(|holder| holder.published(&current))
    {
        return Err(Error::internal_unexpected(format!(
            "daemon lease `{}` changed before its service-managed restart; refusing to signal pid {}",
            state.lease_id, state.pid
        )));
    }
    crate::process::signal_pid(state.pid, crate::process::SIGNAL_TERMINATE)
}

/// Stop the exact stale lease. A concurrent caller may already have replaced
/// it; only a stop failure that leaves this same lease serving is an error.
fn restart_in_place(lease_id: &str) -> Result<()> {
    match super::stop_for_lease(lease_id) {
        Ok(_) => Ok(()),
        Err(error) => {
            let current = read_status()?;
            if current.running
                && current
                    .state
                    .is_some_and(|state| state.lease_id == lease_id)
            {
                Err(error)
            } else {
                Ok(())
            }
        }
    }
}

/// The optional controller operation id is intentionally additive. Existing
/// callers retain the historical ensure-running behavior; controller retries
/// reuse the durable startup token so a lost response cannot create C.
pub fn ensure_running_with_replacement_operation(
    addr: &str,
    replacement_operation_id: Option<&str>,
) -> Result<DaemonStartResult> {
    let Some(operation_id) = replacement_operation_id.filter(|id| !id.trim().is_empty()) else {
        return ensure_running(addr);
    };
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock_for_ensure(ENSURE_RUNNING_STARTUP_WAIT)?;
    let status = read_status()?;
    if let Some(state) = status
        .state
        .filter(|state| state.startup_token == operation_id)
    {
        if pid_is_running(state.pid) {
            return Ok(DaemonStartResult {
                pid: state.pid,
                address: state.address,
                state_path: state.state_path,
                lease_id: state.lease_id,
            });
        }
    }
    // The startup token is persisted in the daemon lease before the response is
    // emitted, making response-loss replay converge on the same live daemon.
    start_or_return_live_unlocked_with_startup_token(addr, operation_id)
}

/// Replace one explicitly identified, provably dead daemon lease. The operation
/// lock covers validation, durable-job reconciliation, and replacement startup.
///
/// The exact `lease_id` is the operator's destructive-action target; PID death
/// is proven here rather than asserted. `adopt_orphaned_lease_with_operations`
/// requires a `PidDead` freshness code, re-proves the recorded PID dead *under
/// the owner lock* (so a reused PID cannot slip through), and refuses when any
/// active child process survives.
pub fn adopt_orphaned_lease(
    lease_id: &str,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
    addr: &str,
) -> Result<DaemonOrphanAdoptionResult> {
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    adopt_orphaned_lease_with_operations(
        lease_id,
        read_status,
        pid_is_running,
        try_acquire_daemon_owner_lock,
        || {
            let store =
                super::JobStore::open_without_reconciliation(crate::paths::observation_db()?)?;
            if confirmed_no_pid_job_ids.is_empty() {
                store.reconcile_dead_daemon_lease_jobs(lease_id)
            } else {
                store.recover_expired_pidless_reservation_for_dead_daemon_lease(
                    lease_id,
                    confirmed_no_pid_job_ids,
                )
            }
        },
        || start_or_return_live_unlocked(addr),
    )
}

/// Explicit recovery for the otherwise irreconcilable case where a proven-dead
/// daemon lost jobs before it could persist a child identity. This never widens
/// automatic adoption: the operator must name every active job and attest that
/// workload processes were inspected and are absent.
///
/// `confirm_workload_processes_absent` is deliberately retained. This command
/// exists precisely because the daemon died before persisting child identity,
/// so the store holds no PID for the named jobs and nothing in this process can
/// observe whether their workloads are still running. The store-side check only
/// proves the *absence of recorded* child evidence — the very condition that
/// makes the operator's inspection the sole source of truth — and it persists
/// the attestation as durable provenance on every affected job.
///
/// PID death, by contrast, is proven here:
/// `reconcile_dead_lease_orphans_with_operations` requires a `PidDead` freshness
/// code, a non-running recorded PID, and a second liveness proof taken under
/// the owner lock. Launcher termination evidence is retained when it proves this
/// lease, but is not a prerequisite because the launcher may be the component
/// that was lost. The operator attestation, exact active-job compare-and-swap,
/// and owner probe are the explicit authority for that gap.
pub fn reconcile_dead_lease_orphans(
    lease_id: &str,
    job_ids: &[uuid::Uuid],
    confirm_workload_processes_absent: bool,
    addr: &str,
    start_replacement: bool,
) -> Result<DaemonExactOrphanRecoveryResult> {
    if !confirm_workload_processes_absent {
        return Err(Error::validation_invalid_argument("confirm_workload_processes_absent", "exact dead-lease recovery requires --confirm-workload-processes-absent after inspecting workload processes", None, None));
    }
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    reconcile_dead_lease_orphans_with_operations(
        lease_id,
        DeadLeaseOrphanRecoveryOperations {
            status: read_status,
            pid_is_running,
            acquire_owner: try_acquire_daemon_owner_lock,
            prove_no_owner: || Ok(owner_lock_proof()),
            reconcile: |pid| {
                let store =
                    super::JobStore::open_without_reconciliation(crate::paths::observation_db()?)?;
                store.reconcile_exact_daemon_loss_jobs(lease_id, job_ids, pid)
            },
            // A remote attested reconcile only terminalizes one dead lease's
            // exact job set; the controller's own reconcile owns what serves
            // next (#15556), so it may ask for no replacement here.
            start: || {
                if start_replacement {
                    start_or_return_live_unlocked(addr).map(Some)
                } else {
                    Ok(None)
                }
            },
        },
    )
}

struct DeadLeaseOrphanRecoveryOperations<
    Status,
    PidIsRunning,
    AcquireOwner,
    ProveNoOwner,
    Reconcile,
    Start,
> {
    status: Status,
    pid_is_running: PidIsRunning,
    acquire_owner: AcquireOwner,
    prove_no_owner: ProveNoOwner,
    reconcile: Reconcile,
    start: Start,
}

fn reconcile_dead_lease_orphans_with_operations<
    Status,
    PidIsRunning,
    AcquireOwner,
    OwnerLock,
    ProveNoOwner,
    Reconcile,
    Start,
>(
    lease_id: &str,
    operations: DeadLeaseOrphanRecoveryOperations<
        Status,
        PidIsRunning,
        AcquireOwner,
        ProveNoOwner,
        Reconcile,
        Start,
    >,
) -> Result<DaemonExactOrphanRecoveryResult>
where
    Status: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: Fn(u32) -> bool,
    AcquireOwner: FnOnce() -> Result<Option<OwnerLock>>,
    ProveNoOwner: FnOnce() -> Result<Vec<String>>,
    Reconcile: FnOnce(u32) -> Result<crate::api_jobs::DaemonLeaseJobDiagnostics>,
    Start: FnOnce() -> Result<Option<super::DaemonStartResult>>,
{
    let DeadLeaseOrphanRecoveryOperations {
        status,
        pid_is_running,
        acquire_owner,
        prove_no_owner,
        reconcile,
        start,
    } = operations;
    let status = status()?;
    let state = status.state.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "exact dead-lease recovery requires a persisted daemon lease",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    if state.lease_id != lease_id {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "recorded daemon lease does not match requested dead lease",
            Some(lease_id.to_string()),
            None,
        ));
    }
    if status.freshness.stale_reason_code != Some(DaemonStaleReasonCode::PidDead)
        || pid_is_running(state.pid)
    {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "recorded daemon PID is live or not proven dead",
            Some(lease_id.to_string()),
            None,
        ));
    }
    let termination = status.termination_evidence.filter(|evidence| {
        evidence.classification == DaemonTerminationClassification::UnexpectedExit
            && !evidence.stop_requested
            && evidence.lease_id.as_deref() == Some(lease_id)
            && evidence.pid == Some(state.pid)
    });
    let owner_lock = acquire_owner()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "daemon owner lock is held; refusing exact dead-lease recovery",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    if pid_is_running(state.pid) {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "recorded daemon PID became live or was reused during recovery",
            Some(lease_id.to_string()),
            None,
        ));
    }
    let mut ownership_proof = prove_no_owner()?;
    ownership_proof.push(
        "operator supplied --confirm-workload-processes-absent; current lease and dead PID were re-proven under the owner lock".to_string(),
    );
    let reconciled = reconcile(state.pid)?;
    drop(owner_lock);
    let replacement = start()?;
    Ok(DaemonExactOrphanRecoveryResult {
        recovered_lease_id: lease_id.to_string(),
        dead_pid: state.pid,
        reconciled_job_ids: reconciled.matching_job_ids,
        termination_evidence: termination,
        ownership_proof,
        replacement,
    })
}

fn adopt_orphaned_lease_with_operations<
    Status,
    PidIsRunning,
    AcquireOwner,
    OwnerLock,
    Reconcile,
    Start,
>(
    lease_id: &str,
    status: Status,
    pid_is_running: PidIsRunning,
    acquire_owner: AcquireOwner,
    reconcile: Reconcile,
    start: Start,
) -> Result<DaemonOrphanAdoptionResult>
where
    Status: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: Fn(u32) -> bool,
    AcquireOwner: FnOnce() -> Result<Option<OwnerLock>>,
    Reconcile: FnOnce() -> Result<crate::api_jobs::DaemonLeaseJobDiagnostics>,
    Start: FnOnce() -> Result<super::DaemonStartResult>,
{
    let status = status()?;
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

    let owner_lock = acquire_owner()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "daemon owner lock is held; refusing exact dead-lease adoption",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    // The held owner lock is the no-owner proof: no daemon serves this store.
    // Revalidate after taking the lifecycle-critical lock: a PID can be reused
    // between status inspection and exact orphan adoption.
    if !pid_is_proven_dead(state.pid, &pid_is_running) {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!("recorded daemon PID {} is live or has been reused", state.pid),
            Some(lease_id.to_string()),
            Some(vec!["Refusing adoption until the exact recorded PID is proven dead under the lifecycle lock.".to_string()]),
        ));
    }
    let reconciled = reconcile()?;
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
    let replacement = start()?;
    Ok(DaemonOrphanAdoptionResult {
        adopted_lease_id: lease_id.to_string(),
        dead_pid: state.pid,
        active_jobs_terminalized: reconciled.terminalized_count(),
        retry_guidance: "Inspect the retained job events, then retry eligible work through its original command or workflow.".to_string(),
        replacement,
    })
}

/// Explicitly recover durable jobs when no daemon owner can be proven. This
/// covers missing lease metadata and stale version-mismatched daemons whose
/// typed `/jobs` view no longer accounts for their durable active jobs.
///
/// Absence of a daemon owner is proven, not asserted: the daemon owner lock is
/// refused while any daemon is live or starting, and only a lock holder serves
/// the store.
pub fn reconcile_leaseless_orphans(
    addr: &str,
    replacement_operation_id: Option<&str>,
) -> Result<DaemonLeaselessRecoveryResult> {
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    let status = read_status()?;
    if let Some(result) = replay_leaseless_recovery(&status, addr, replacement_operation_id)? {
        return Ok(result);
    }
    if status.freshness.active_jobs == 0
        || !matches!(
            status.freshness.stale_reason_code,
            Some(
                DaemonStaleReasonCode::LeaseMissing
                    | DaemonStaleReasonCode::LeaseCorrupt
                    | DaemonStaleReasonCode::VersionMismatch
            )
        )
    {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "recovery requires active jobs with missing, corrupt, or version-mismatched daemon freshness; use exact lease recovery for recorded dead leases",
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
    let ownership_proof = owner_lock_proof();
    let jobs_path = crate::paths::observation_db()?;
    let snapshot_path = snapshot_job_store(&jobs_path)?;
    let store = super::JobStore::open_without_reconciliation(&jobs_path)?;
    let receipt_path = crate::paths::daemon_leaseless_recovery_receipt_file()?;
    let mut receipt = LeaselessRecoveryReceipt {
        affected_job_ids: Vec::new(),
        affected_jobs: Vec::new(),
        historical_lease_ids: Vec::new(),
        evidence_snapshot_path: snapshot_path.display().to_string(),
        ownership_proof: ownership_proof.clone(),
        phase: StateLossRecoveryPhase::Prepared,
        replacement: None,
        replacement_startup_token: None,
        replacement_operation_id: replacement_operation_id.map(str::to_string),
    };
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
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
    if !reconciled.preserved_remote_job_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            format!(
                "deferred lease-less recovery because {} broker-owned remote job(s) remain active or unexpired: {}",
                reconciled.preserved_remote_job_ids.len(),
                reconciled.preserved_remote_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            ),
            None,
            Some(vec!["Wait for each broker-owned claim to expire or reach a terminal state, then retry recovery.".to_string()]),
        ));
    }
    let affected_job_count = reconciled.reconciled_count();
    drop(owner_lock);
    receipt = LeaselessRecoveryReceipt {
        affected_job_ids: reconciled.reconciled_job_ids,
        affected_jobs: reconciled.affected_jobs,
        historical_lease_ids: reconciled.historical_lease_ids,
        evidence_snapshot_path: snapshot_path.display().to_string(),
        ownership_proof,
        phase: StateLossRecoveryPhase::Reconciled,
        replacement: None,
        replacement_startup_token: None,
        replacement_operation_id: replacement_operation_id.map(str::to_string),
    };
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    let startup_token = replacement_operation_id
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    receipt.phase = StateLossRecoveryPhase::ReplacementStarting;
    receipt.replacement_startup_token = Some(startup_token.clone());
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    let replacement = start_or_return_live_unlocked_with_startup_token(addr, &startup_token)?;
    let status = read_status()?;
    if !status.admits_work()
        || status
            .state
            .as_ref()
            .is_none_or(|state| state.startup_token != startup_token)
    {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "replacement startup did not publish the expected lease-less recovery startup token",
            None,
            None,
        ));
    }
    receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
    receipt.replacement = Some(replacement);
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    let mut result = receipt.into_result()?;
    result.affected_job_count = affected_job_count;
    result.retry_guidance = "Recorded job output and artifacts were retained. Retry eligible work through its original command or workflow.".to_string();
    Ok(result)
}

/// The free owner lock, already held by the caller, is the proof that no
/// daemon owns this store: only a lock holder serves it.
fn owner_lock_proof() -> Vec<String> {
    vec!["daemon owner lock acquired non-destructively; no daemon serves this store".to_string()]
}

fn pid_is_proven_dead(pid: u32, is_running: impl FnOnce(u32) -> bool) -> bool {
    !is_running(pid)
}

/// Write the durable job ledger as one JSON evidence document beside the
/// daemon lease, before recovery mutates it.
fn snapshot_job_store(path: &Path) -> Result<PathBuf> {
    let raw = super::JobStore::ledger_snapshot_at_path(path)?;
    let snapshot = crate::paths::daemon_state_file()?.with_file_name(format!(
        "daemon-jobs.leaseless-orphan-{}.snapshot.json",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&snapshot, raw).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("snapshot {}", snapshot.display())),
        )
    })?;
    Ok(snapshot)
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
    start_or_return_live_unlocked_with_startup_token(addr, &uuid::Uuid::new_v4().to_string())
}

fn start_or_return_live_unlocked_with_startup_token(
    addr: &str,
    startup_token: &str,
) -> Result<DaemonStartResult> {
    start_or_return_live_unlocked_with_idle_timeout(
        addr,
        startup_token,
        super::lifetime::DEFAULT_IDLE_TIMEOUT_SECS,
    )
}

fn start_or_return_live_unlocked_with_idle_timeout(
    addr: &str,
    startup_token: &str,
    default_idle_timeout: u64,
) -> Result<DaemonStartResult> {
    let idle_timeout = super::lifetime::launch_idle_timeout(default_idle_timeout)?;
    start_or_return_live_with_operations(
        read_status,
        try_acquire_daemon_owner_lock,
        stop_stale_daemon_for_start,
        || spawn_and_wait_for_lease_attempt(addr, startup_token, idle_timeout, true, Vec::new()),
    )
}

/// Startup cleanup can stop a stale daemon, so it must not pass a Cook that
/// has verified that daemon but not submitted.
fn stop_stale_daemon_for_start() -> Result<()> {
    let _admission_fence = acquire_daemon_job_admission_fence()?;
    stop_unlocked().map(|_| ())
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
    // A live-but-stale daemon must not be returned as if it were usable:
    // `running` is pure process liveness, so the start path gates on the
    // full live/reachable/current-build predicate before attaching (#13621).
    if existing.admits_work() {
        if let Some(state) = existing.state {
            return Ok(DaemonStartResult {
                pid: state.pid,
                address: state.address,
                state_path: state.state_path,
                lease_id: state.lease_id,
            });
        }
    }
    // A held owner lock is a live or starting daemon, whatever the lease says:
    // for example a service manager restarting its own. Spawning beside it
    // would only contend for the lock, so no path starts one (#17387).
    let owner_lock = acquire_owner()?.ok_or_else(|| owner_held_error(&existing))?;
    if existing.state.is_some() || existing.stale_reason.is_some() {
        cleanup()?;
    }
    drop(owner_lock);

    spawn_and_wait()
}

fn owner_held_error(status: &super::DaemonStatus) -> Error {
    match status.service_manager() {
        Some(manager) => Error::internal_unexpected(format!(
            "the daemon is owned by service manager {}; refusing to start another daemon beside it",
            manager.as_str()
        ))
        .with_hint(
            "Restart it through its service manager, or run `homeboy daemon ensure-running`, which waits for the manager's replacement."
                .to_string(),
        ),
        None => Error::internal_unexpected(
            "daemon owner lock is held by a live or starting daemon; refusing to start another daemon",
        ),
    }
}

const STARTUP_LEASE_OBSERVATIONS: usize = 100;
const STARTUP_LEASE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupLeaseObservation {
    observed_pid: Option<u32>,
    observed_lease_id: Option<String>,
    observed_token: Option<String>,
    observed_running: Option<bool>,
    observed_fresh: Option<bool>,
    observed_reachable: Option<bool>,
}

/// A launch token is an attempt identity, never a general daemon readiness
/// signal. The spawned process is the daemon itself, but a PID alone can be
/// reused, so the live lease is identified by its unique token.
fn observe_startup_lease<ReadStatus, Sleep>(
    _launcher_pid: u32,
    startup_token: &str,
    observations: usize,
    mut read_status: ReadStatus,
    mut sleep: Sleep,
) -> Result<std::result::Result<DaemonStartResult, StartupLeaseObservation>>
where
    ReadStatus: FnMut() -> Result<super::DaemonStatus>,
    Sleep: FnMut(),
{
    let mut observed = StartupLeaseObservation {
        observed_pid: None,
        observed_lease_id: None,
        observed_token: None,
        observed_running: None,
        observed_fresh: None,
        observed_reachable: None,
    };
    for attempt in 0..=observations {
        let status = read_status()?;
        let admits_work = status.admits_work();
        observed.observed_running = Some(status.running);
        observed.observed_fresh = Some(status.fresh);
        observed.observed_reachable = Some(status.reachable);
        if let Some(state) = status.state {
            if admits_work && state.startup_token == startup_token {
                return Ok(Ok(DaemonStartResult {
                    pid: state.pid,
                    address: state.address,
                    state_path: state.state_path,
                    lease_id: state.lease_id,
                }));
            }
            observed.observed_pid = Some(state.pid);
            observed.observed_lease_id = Some(state.lease_id);
            observed.observed_token =
                (!state.startup_token.is_empty()).then_some(state.startup_token);
        }
        if attempt < observations {
            sleep();
        }
    }
    Ok(Err(observed))
}

struct StartupAttempt {
    child: std::process::Child,
    token: String,
    state_path: PathBuf,
    armed: bool,
}

impl StartupAttempt {
    fn new(child: std::process::Child, token: &str, state_path: &Path) -> Self {
        Self {
            child,
            token: token.to_string(),
            state_path: state_path.to_path_buf(),
            armed: true,
        }
    }

    fn cleanup(&mut self) -> Vec<String> {
        self.armed = false;
        let evidence =
            cleanup_startup_attempt_at_path(self.child.id(), &self.token, &self.state_path)
                .unwrap_or_else(|error| {
                    vec![format!("startup cleanup could not complete: {error}")]
                });
        let _ = self.child.try_wait();
        evidence
    }

    fn finish(&mut self, result: Result<DaemonStartResult>) -> Result<DaemonStartResult> {
        match result {
            Ok(result) => {
                self.armed = false;
                Ok(result)
            }
            Err(mut error) => {
                if self.armed {
                    error.details["startup_cleanup"] = serde_json::json!(self.cleanup());
                }
                Err(error)
            }
        }
    }
}

impl Drop for StartupAttempt {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.cleanup();
        }
    }
}

fn cleanup_startup_attempt_at_path(
    pid: u32,
    startup_token: &str,
    state_path: &Path,
) -> Result<Vec<String>> {
    let mut cleanup = Vec::new();
    let state = match super::validate_lease_file(state_path) {
        Ok(validation) => validation.state,
        Err(error) => {
            cleanup.push(format!("startup lease could not be inspected: {error}"));
            None
        }
    };
    if let Some(state) = state.filter(|state| state.startup_token == startup_token) {
        let identity = super::DaemonLeaseIdentity::from_state(&state);
        if !pid_is_running(state.pid) {
            super::remove_lease_if_identity_matches(state_path, &identity)?;
            cleanup.push(format!(
                "startup cleanup observed an exit race and removed the stale token lease for pid {}",
                state.pid
            ));
        } else if pid_has_ownership_token(state.pid, DAEMON_STARTUP_TOKEN_ENV, startup_token)? {
            match terminate_token_owned_startup_process(state.pid, startup_token) {
                Ok(StartupCleanupOutcome::Terminated(signal)) => {
                    super::remove_lease_if_identity_matches(state_path, &identity)?;
                    cleanup.push(format!(
                        "terminated token-owned daemon pid {} with {signal}",
                        state.pid
                    ));
                }
                Ok(StartupCleanupOutcome::ExitedBeforeEscalation) => cleanup.push(format!(
                    "startup cleanup observed an exit race for daemon pid {}; no SIGKILL sent",
                    state.pid
                )),
                Ok(StartupCleanupOutcome::OwnershipLost) => cleanup.push(format!(
                    "did not signal daemon pid {} after live token ownership was lost",
                    state.pid
                )),
                Err(error) => cleanup.push(format!(
                    "retained lease for pid {} after cleanup race: {error}",
                    state.pid
                )),
            }
        } else {
            cleanup.push(format!(
                "retained lease for pid {} because live token ownership could not be proven",
                state.pid
            ));
        }
    }
    if pid_is_running(pid) {
        if pid_has_ownership_token(pid, DAEMON_STARTUP_TOKEN_ENV, startup_token)? {
            match terminate_token_owned_startup_process(pid, startup_token) {
                Ok(StartupCleanupOutcome::Terminated(signal)) => cleanup.push(format!(
                    "terminated token-owned launcher pid {pid} with {signal}"
                )),
                Ok(StartupCleanupOutcome::ExitedBeforeEscalation) => cleanup.push(format!(
                    "startup cleanup observed an exit race for launcher pid {pid}; no SIGKILL sent"
                )),
                Ok(StartupCleanupOutcome::OwnershipLost) => cleanup.push(format!(
                    "did not signal launcher pid {pid} after live token ownership was lost"
                )),
                Err(error) => cleanup.push(format!(
                    "retained launcher pid {pid} after cleanup race: {error}"
                )),
            }
        } else {
            cleanup.push(format!(
                "did not signal launcher pid {pid}: live token ownership could not be proven"
            ));
        }
    } else {
        cleanup.push(format!(
            "startup cleanup observed an exit race for launcher pid {pid}; no signal sent"
        ));
    }
    Ok(cleanup)
}

fn terminate_token_owned_startup_process(
    pid: u32,
    startup_token: &str,
) -> Result<StartupCleanupOutcome> {
    terminate_token_owned_startup_process_with_outcome(
        pid,
        startup_token,
        super::FORCE_STOP_WAIT,
        |pid, token| pid_has_ownership_token(pid, DAEMON_STARTUP_TOKEN_ENV, token),
        pid_is_running,
        signal_pid,
        wait_for_pid_exit,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupCleanupOutcome {
    Terminated(&'static str),
    ExitedBeforeEscalation,
    OwnershipLost,
}

fn terminate_token_owned_startup_process_with_outcome<Owns, IsRunning, Signal, Wait>(
    pid: u32,
    startup_token: &str,
    timeout: Duration,
    mut owns_token: Owns,
    mut is_running: IsRunning,
    mut signal: Signal,
    mut wait_for_exit: Wait,
) -> Result<StartupCleanupOutcome>
where
    Owns: FnMut(u32, &str) -> Result<bool>,
    IsRunning: FnMut(u32) -> bool,
    Signal: FnMut(u32, libc::c_int) -> Result<()>,
    Wait: FnMut(u32, Duration) -> bool,
{
    if !owns_token(pid, startup_token)? {
        return Err(Error::validation_invalid_argument(
            "daemon_startup_cleanup",
            format!("process {pid} no longer owns the daemon startup token"),
            Some(pid.to_string()),
            None,
        ));
    }

    signal(pid, SIGNAL_TERMINATE)?;
    if wait_for_exit(pid, timeout) {
        return Ok(StartupCleanupOutcome::Terminated("SIGTERM"));
    }

    // A process can exit after the grace-window observation, or the PID can be
    // reused before the second ownership check. Only a still-live, still-owned
    // process may receive SIGKILL.
    if !is_running(pid) {
        return Ok(StartupCleanupOutcome::ExitedBeforeEscalation);
    }
    if !owns_token(pid, startup_token)? {
        return Ok(StartupCleanupOutcome::OwnershipLost);
    }

    signal(pid, SIGNAL_KILL)?;
    if wait_for_exit(pid, timeout) {
        Ok(StartupCleanupOutcome::Terminated("SIGKILL"))
    } else {
        Err(Error::internal_unexpected(format!(
            "startup process {pid} survived bounded SIGTERM-to-SIGKILL escalation"
        )))
    }
}

#[cfg(test)]
fn terminate_token_owned_startup_process_with_operations<Owns, Signal, Wait>(
    pid: u32,
    startup_token: &str,
    timeout: Duration,
    mut owns_token: Owns,
    mut signal: Signal,
    mut wait_for_exit: Wait,
) -> Result<&'static str>
where
    Owns: FnMut(u32, &str) -> Result<bool>,
    Signal: FnMut(u32, libc::c_int) -> Result<()>,
    Wait: FnMut(u32, Duration) -> bool,
{
    match terminate_token_owned_startup_process_with_outcome(
        pid,
        startup_token,
        timeout,
        &mut owns_token,
        |_| true,
        &mut signal,
        &mut wait_for_exit,
    )? {
        StartupCleanupOutcome::Terminated(signal) => Ok(signal),
        StartupCleanupOutcome::ExitedBeforeEscalation => Err(Error::validation_invalid_argument(
            "daemon_startup_cleanup",
            format!("process {pid} exited before SIGKILL escalation"),
            Some(pid.to_string()),
            None,
        )),
        StartupCleanupOutcome::OwnershipLost => Err(Error::validation_invalid_argument(
            "daemon_startup_cleanup",
            format!(
                "process {pid} lost daemon startup-token ownership before SIGKILL; refusing to signal"
            ),
            Some(pid.to_string()),
            None,
        )),
    }
}

/// Build the timeout report from what startup observation actually saw.
///
/// `observe_startup_lease` admits on the startup token, so the common cause of
/// a timeout is the daemon never becoming work-admitting (`running && fresh &&
/// reachable`) — usually because the process exited. A token or PID mismatch is
/// asserted only when one was actually observed. `daemon_log` is the bounded,
/// redacted tail of what this launch wrote to `daemon.log`.
fn startup_timeout_error(
    pid: u32,
    startup_token: &str,
    observation: StartupLeaseObservation,
    cleanup: Vec<String>,
    daemon_log: Option<String>,
) -> Error {
    let token_mismatch = observation
        .observed_token
        .as_deref()
        .is_some_and(|token| token != startup_token);
    let mut failed = Vec::new();
    if observation.observed_running == Some(false) {
        failed.push("the daemon process was not running");
    }
    if observation.observed_fresh == Some(false) {
        failed.push("the daemon binary was stale (did not match this Homeboy build)");
    }
    if observation.observed_reachable == Some(false) {
        failed.push("the daemon API was not reachable");
    }
    if token_mismatch {
        failed.push("the lease was held under a different startup token");
    }
    let breakdown = if failed.is_empty() {
        "the live daemon lease never matched this startup attempt".to_string()
    } else {
        failed.join("; ")
    };
    let mut error = Error::internal_unexpected(format!(
        "daemon startup did not reach a work-admitting lease before timeout: {breakdown}"
    ))
    .with_hint("The daemon startup never reached the live, fresh, and reachable state required to accept work. Retry the Lab Cook; provider budget was not consumed.".to_string());
    error.details = serde_json::json!({
        "classification": "terminal_pre_provider_startup",
        "admission": {
            "running": observation.observed_running,
            "fresh": observation.observed_fresh,
            "reachable": observation.observed_reachable,
            "startup_token_mismatch": token_mismatch,
        },
        "daemon_log": daemon_log,
        "expected": { "pid": pid, "startup_token": startup_token },
        "observed": {
            "pid": observation.observed_pid,
            "lease_id": observation.observed_lease_id,
            "startup_token": observation.observed_token,
        },
        "cleanup": cleanup,
        "managed_recovery_actions": 1,
        "provider_budget_consumed": false,
        "safe_next_action": "Retry the same Lab Cook. Inspect `homeboy daemon status` if the failure repeats.",
    });
    error
}

fn can_recover_startup_attempt(
    allow_retry: bool,
    startup_token: &str,
    observation: &StartupLeaseObservation,
    cleanup_evidence: &[String],
) -> bool {
    allow_retry
        && observation
            .observed_token
            .as_deref()
            .is_none_or(|token| token == startup_token)
        && !cleanup_evidence
            .iter()
            .any(|entry| entry.contains("could not be proven"))
}

fn spawn_and_wait_for_lease_attempt(
    addr: &str,
    startup_token: &str,
    idle_timeout: u64,
    allow_retry: bool,
    mut cleanup_evidence: Vec<String>,
) -> Result<DaemonStartResult> {
    let state_path = crate::paths::daemon_state_file()?;
    let state_dir = state_path.parent().expect("daemon state parent");
    // `serve` records its startup path as the binary to relaunch after an
    // upgrade replaces it, so launch from this process's recorded startup path.
    let exe = super::lifetime::launchable_executable().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let (log, daemon_log) = DaemonLog::open(state_dir)?;
    let log_stderr = log.try_clone().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("open {}", daemon_log.path.display())),
        )
    })?;
    let mut command = Command::new(exe);
    // The spawned process is the daemon itself: its PID is the lease PID. The
    // startup token in argv is the portable, exact ownership proof used when a
    // platform cannot inspect a process environment.
    command
        .args([
            "daemon",
            "serve",
            "--addr",
            addr,
            "--startup-token",
            startup_token,
            "--state-dir",
        ])
        .arg(state_dir)
        .env(DAEMON_STARTUP_TOKEN_ENV, startup_token)
        .env(super::lifetime::IDLE_TIMEOUT_ENV, idle_timeout.to_string())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_stderr);
    // A service manager's variables leak into this launcher when it runs under
    // a unit. Once the launcher exits the detached daemon is reparented, which
    // can be to that manager; it must still never read as service-managed.
    for name in super::service_manager::DaemonServiceManager::ENV_VARS {
        command.env_remove(name);
    }
    super::lifetime::LauncherIdentity::configure(&mut command);
    detach_from_launcher_session(&mut command);
    let child = command
        .spawn()
        .map_err(|e| Error::internal_io(e.to_string(), Some("spawn daemon".to_string())))?;
    let pid = child.id();
    let mut attempt = StartupAttempt::new(child, startup_token, &state_path);
    let result = (|| {
        match observe_startup_lease(
            pid,
            startup_token,
            STARTUP_LEASE_OBSERVATIONS,
            || super::read_status_for_state_path(state_path.clone()),
            || thread::sleep(STARTUP_LEASE_POLL),
        )? {
            Ok(result) => Ok(result),
            Err(observation) => {
                // Cleanup is evidence about the failed attempt, not a replacement
                // for the startup failure that caused this control path.
                let cleanup = attempt.cleanup();
                cleanup_evidence.extend(cleanup);
                if can_recover_startup_attempt(
                    allow_retry,
                    startup_token,
                    &observation,
                    &cleanup_evidence,
                ) {
                    // The token is the durable admission identity for this
                    // startup. A bounded recovery restarts only the same proven
                    // attempt, so receipt replay and concurrent callers can never
                    // mistake a replacement for a second daemon.
                    return spawn_and_wait_for_lease_attempt(
                        addr,
                        startup_token,
                        idle_timeout,
                        false,
                        cleanup_evidence,
                    );
                }
                Err(startup_timeout_error(
                    pid,
                    startup_token,
                    observation,
                    cleanup_evidence,
                    daemon_log.tail(),
                ))
            }
        }
    })();
    attempt.finish(result)
}

/// File in the state directory that receives the detached daemon's stdout and
/// stderr, appended across launches.
pub(super) const DAEMON_LOG_FILE: &str = "daemon.log";
/// `daemon.log` is not rotated; a launch truncates it once it exceeds this.
const DAEMON_LOG_TRUNCATE_BYTES: u64 = 4 * 1024 * 1024;
/// How far back from its end startup diagnostics read `daemon.log`.
const DAEMON_LOG_TAIL_READ_BYTES: u64 = 64 * 1024;

/// The part of `daemon.log` written by one launch.
struct DaemonLog {
    path: PathBuf,
    start: u64,
}

impl DaemonLog {
    /// Open `daemon.log` for appending, truncating it first when it has grown
    /// past [`DAEMON_LOG_TRUNCATE_BYTES`].
    fn open(state_dir: &Path) -> Result<(std::fs::File, Self)> {
        let path = state_dir.join(DAEMON_LOG_FILE);
        let io_error = |error: std::io::Error| {
            Error::internal_io(error.to_string(), Some(format!("open {}", path.display())))
        };
        std::fs::create_dir_all(state_dir).map_err(io_error)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_error)?;
        let mut start = file.metadata().map_err(io_error)?.len();
        if start > DAEMON_LOG_TRUNCATE_BYTES {
            file.set_len(0).map_err(io_error)?;
            start = 0;
        }
        Ok((file, Self { path, start }))
    }

    /// Bounded, redacted tail of what this launch wrote, or `None` when it
    /// wrote nothing readable.
    fn tail(&self) -> Option<String> {
        use std::io::{BufRead, Seek};
        let mut file = std::fs::File::open(&self.path).ok()?;
        let end = file.metadata().ok()?.len();
        let from = self
            .start
            .max(end.saturating_sub(DAEMON_LOG_TAIL_READ_BYTES));
        if from >= end {
            return None;
        }
        file.seek(std::io::SeekFrom::Start(from)).ok()?;
        let mut reader = std::io::BufReader::new(file.take(end - from));
        if from > self.start {
            // The read began inside a record; its key may be cut off, so the
            // record cannot be redacted reliably. Drop it.
            reader.read_until(b'\n', &mut Vec::new()).ok()?;
        }
        bounded_redacted_reader(reader)
    }
}

/// Keep the daemon and its workload children alive when a transient launcher
/// connection, such as direct SSH, disconnects.
fn detach_from_launcher_session(command: &mut Command) {
    // The hermetic Cargo runner owns the test binary's process group and reaps
    // it on cancellation. Test daemons must remain in that group; a `setsid`
    // here would reparent a cancelled fixture to PID 1 and leak it forever.
    // The value names that group; rejoin it rather than staying in the
    // launching CLI's own group, which its test harness reaps when the CLI
    // exits (#17387).
    if cfg!(debug_assertions) {
        if let Some(group) = std::env::var_os(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV) {
            join_test_runner_process_group(command, &group);
            return;
        }
    }
    crate::process::detach_from_caller_session(command);
}

#[cfg(unix)]
fn join_test_runner_process_group(command: &mut Command, group: &std::ffi::OsStr) {
    use std::os::unix::process::CommandExt;
    if group == TEST_KEEP_DAEMON_IN_LAUNCHER_GROUP {
        return;
    }
    let group = group
        .to_str()
        .and_then(|group| group.parse::<libc::pid_t>().ok())
        .filter(|group| *group > 0)
        .unwrap_or_else(|| panic!("invalid test daemon process group {group:?}"));
    // SAFETY: `pre_exec` runs in the forked child immediately before exec;
    // `setpgid` only moves that child and reports failure through errno.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, group) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn join_test_runner_process_group(_command: &mut Command, _group: &std::ffi::OsStr) {}

/// Resolve the daemon base URL, falling back to the running daemon's address.
fn resolve_daemon_url(daemon_url: Option<String>) -> Result<String> {
    if let Some(url) = daemon_url.filter(|url| !url.trim().is_empty()) {
        return Ok(url);
    }
    let status = read_status()?;
    let admits_work = status.admits_work();
    let Some(state) = status.state.filter(|_| admits_work) else {
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

#[cfg(test)]
mod termination_tests {
    use super::*;

    #[test]
    fn termination_output_reader_retains_only_a_bounded_redacted_tail() {
        let mut bytes = vec![b'x'; 8 * 1024 * 1024];
        bytes.extend_from_slice(b"\ntoken=super-secret\nfinal diagnostic");
        let output = bounded_redacted_reader(std::io::Cursor::new(bytes)).expect("output");
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("[truncated]"));
        assert!(output.contains("final diagnostic"));
        assert!(output.len() < 4_200);
    }

    #[test]
    fn output_reader_redacts_a_secret_split_across_read_boundaries() {
        struct ChunkedReader {
            bytes: Vec<u8>,
            offset: usize,
        }

        impl Read for ChunkedReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.offset == self.bytes.len() {
                    return Ok(0);
                }
                let count = 3.min(self.bytes.len() - self.offset).min(buffer.len());
                buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
                self.offset += count;
                Ok(count)
            }
        }

        let output = bounded_redacted_reader(ChunkedReader {
            bytes: b"before token=boundary-secret after\n".to_vec(),
            offset: 0,
        })
        .expect("output");

        assert!(output.contains("token=[REDACTED]"));
        assert!(!output.contains("boundary-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn output_reader_drains_sustained_os_pipe_output_with_a_bounded_tail() {
        let mut child = Command::new("sh")
            .args([
                "-c",
                "i=0; while [ $i -lt 8192 ]; do printf 'diagnostic-%s\\n' \"$i\"; i=$((i + 1)); done; printf 'token=pipe-secret\\n'",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("output fixture");
        let output =
            bounded_redacted_reader(child.stdout.take().expect("fixture stdout")).expect("output");
        assert!(child.wait().expect("reap fixture").success());
        assert!(output.contains("token=[REDACTED]"));
        assert!(!output.contains("pipe-secret"));
        assert!(output.len() <= 4096);
    }

    #[test]
    fn output_reader_preserves_bounded_read_failure_diagnostics() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("token=read-secret"))
            }
        }

        let output = bounded_redacted_reader(FailingReader).expect("diagnostic");
        assert!(output.contains("output read failed"));
        assert!(!output.contains("read-secret"));
    }

    #[test]
    fn daemon_log_tail_reads_only_this_launch_and_redacts_it() {
        let dir = tempfile::tempdir().expect("state dir");
        std::fs::write(
            dir.path().join(DAEMON_LOG_FILE),
            b"token=previous-launch-secret\n",
        )
        .expect("prior log");
        let (mut log, daemon_log) = DaemonLog::open(dir.path()).expect("open log");
        assert_eq!(
            daemon_log.tail(),
            None,
            "a launch that wrote nothing has no tail"
        );
        std::io::Write::write_all(&mut log, b"error: bind failed token=launch-secret\n")
            .expect("write log");

        let tail = daemon_log.tail().expect("tail");
        assert!(tail.contains("error: bind failed"));
        assert!(tail.contains("token=[REDACTED]"));
        assert!(!tail.contains("secret"));
    }

    #[test]
    fn daemon_log_tail_drops_a_record_cut_by_its_read_window() {
        let dir = tempfile::tempdir().expect("state dir");
        let (mut log, daemon_log) = DaemonLog::open(dir.path()).expect("open log");
        let mut bytes = b"token=".to_vec();
        bytes.extend(std::iter::repeat_n(
            b'x',
            DAEMON_LOG_TAIL_READ_BYTES as usize,
        ));
        bytes.extend_from_slice(b"\nfinal diagnostic\n");
        std::io::Write::write_all(&mut log, &bytes).expect("write log");

        let tail = daemon_log.tail().expect("tail");
        assert!(tail.contains("final diagnostic"));
        assert!(!tail.contains("xxx"), "a record without its key is dropped");
    }

    #[test]
    fn daemon_log_is_truncated_at_launch_once_it_exceeds_its_bound() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join(DAEMON_LOG_FILE);
        std::fs::write(&path, vec![b'x'; DAEMON_LOG_TRUNCATE_BYTES as usize]).expect("log");
        DaemonLog::open(dir.path()).expect("open bounded log");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            DAEMON_LOG_TRUNCATE_BYTES,
            "a log at its bound is kept"
        );
        std::fs::write(&path, vec![b'x'; DAEMON_LOG_TRUNCATE_BYTES as usize + 1]).expect("log");
        let (_, daemon_log) = DaemonLog::open(dir.path()).expect("open oversized log");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(daemon_log.start, 0);
    }

    #[test]
    fn requested_stop_is_distinct_from_unexpected_exit() {
        assert_ne!(
            DaemonTerminationClassification::CleanStop,
            DaemonTerminationClassification::UnexpectedExit
        );
    }
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
#[path = "../../../../tests/core/daemon/control_test.rs"]
mod control_test;
