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
    DaemonOrphanAdoptionResult, DaemonProcessCandidate, DaemonProcessOwnership,
    DaemonStaleReasonCode, DaemonStartResult, DaemonTerminationClassification,
    DaemonTerminationEvidence, DAEMON_STARTUP_TOKEN_ENV,
};

/// Enumerate foreground daemon processes without inferring ownership from a
/// command substring. A candidate is an owner only when its explicit HOME
/// environment resolves to this durable store and its executable is the active
/// binary; absent evidence remains ambiguous.
pub(super) fn daemon_process_candidates(jobs_path: &Path) -> Result<Vec<DaemonProcessCandidate>> {
    let output = Command::new("ps")
        .args(["-axeww", "-o", "pid=", "-o", "comm=", "-o", "command="])
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("inspect daemon processes".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(
            "unable to inspect daemon processes",
        ));
    }
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| parse_daemon_process_candidate(line, jobs_path, current_exe.as_deref()))
        .collect())
}

/// Supervise one daemon child and persist its bounded termination evidence.
/// This is shared by local and SSH launches because SSH invokes the same CLI.
pub fn supervise(addr: &str, startup_token: &str) -> Result<()> {
    let exe = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let child = Command::new(exe)
        .args(["daemon", "serve", "--addr", addr])
        .env(DAEMON_STARTUP_TOKEN_ENV, startup_token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("spawn supervised daemon".to_string()),
            )
        })?;
    let pid = child.id();
    let output = child.wait_with_output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("wait for supervised daemon".to_string()),
        )
    })?;
    let state = super::validate_lease_file(&crate::core::paths::daemon_state_file()?)
        .ok()
        .and_then(|validation| validation.state);
    let prior = super::read_termination_evidence()?;
    let stop_requested = prior
        .as_ref()
        .is_some_and(|evidence| evidence.stop_requested && evidence.pid == Some(pid));
    let (exit_code, signal) = exit_details(&output.status);
    let evidence = DaemonTerminationEvidence {
        classification: if stop_requested { DaemonTerminationClassification::CleanStop } else { DaemonTerminationClassification::UnexpectedExit },
        observed_at: chrono::Utc::now().to_rfc3339(),
        lease_id: state.as_ref().map(|state| state.lease_id.clone()).or_else(|| prior.and_then(|evidence| evidence.lease_id)),
        pid: Some(pid),
        binary_identity: state.as_ref().map(|state| state.build_identity.display.clone()),
        active_jobs: super::JobStore::active_count_at_path(crate::core::paths::daemon_jobs_file()?)?,
        resource_evidence: "unavailable: launcher does not collect OS resource snapshots".to_string(),
        os_evidence: "unavailable: no OS evidence collected; exit status and signal are launcher observations only".to_string(),
        exit_code, signal,
        stdout: bounded_redacted(&output.stdout), stderr: bounded_redacted(&output.stderr), stop_requested,
    };
    super::write_termination_evidence(&evidence)
}

fn bounded_redacted(bytes: &[u8]) -> Option<String> {
    const LIMIT: usize = 4096;
    if bytes.is_empty() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]).to_string();
    if bytes.len() > LIMIT {
        text.push_str("\n[truncated]");
    }
    Some(crate::core::redaction::redact_string(&text))
}

#[cfg(unix)]
fn exit_details(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    (status.code(), status.signal())
}

#[cfg(not(unix))]
fn exit_details(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), None)
}

fn parse_daemon_process_candidate(
    line: &str,
    jobs_path: &Path,
    current_exe: Option<&Path>,
) -> Option<DaemonProcessCandidate> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let executable = fields.next()?.to_string();
    let cmdline = fields.collect::<Vec<_>>().join(" ");
    if !cmdline.contains("daemon serve") {
        return None;
    }
    let bind_endpoint = cmdline
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| (pair[0] == "--addr").then(|| pair[1].to_string()));
    let home = cmdline
        .split_whitespace()
        .find_map(|part| part.strip_prefix("HOME="));
    let durable_store_path =
        home.map(|home| Path::new(home).join(".config/homeboy/daemon/jobs.json"));
    let executable_matches = current_exe.is_some_and(|current| {
        Path::new(&executable).canonicalize().ok().as_deref() == Some(current)
    });
    let ownership = match durable_store_path.as_deref() {
        Some(store) if store != jobs_path => DaemonProcessOwnership::Unrelated,
        Some(_) if executable_matches => DaemonProcessOwnership::Owning,
        _ => DaemonProcessOwnership::Ambiguous,
    };
    Some(DaemonProcessCandidate {
        pid,
        executable: executable.clone(),
        cmdline: normalize_cmdline(&cmdline),
        bind_endpoint,
        durable_store_path: durable_store_path.map(|path| path.display().to_string()),
        build_identity: executable_matches.then_some("current_executable".to_string()),
        ownership,
    })
}

fn normalize_cmdline(cmdline: &str) -> String {
    cmdline.split_whitespace().collect::<Vec<_>>().join(" ")
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
            replacement_startup_token: None,
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

fn start_state_loss_replacement(
    receipt: &mut StateLossRecoveryReceipt,
    receipt_path: &Path,
    addr: &str,
) -> Result<super::DaemonStateLossRecoveryResult> {
    start_state_loss_replacement_with(receipt, receipt_path, |startup_token| {
        let replacement = start_or_return_live_unlocked_with_startup_token(addr, startup_token)?;
        let status = read_status()?;
        if status.running
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
        if status.running && state.startup_token == token {
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
            "job_store",
            "lease-less reconciliation requires missing or corrupt lease metadata with active jobs",
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
    if !reconciled.preserved_remote_job_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "job_store",
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LeaselessRecoveryReceipt {
    affected_job_ids: Vec<uuid::Uuid>,
    affected_jobs: Vec<crate::core::api_jobs::LeaselessOrphanAffectedJob>,
    historical_lease_ids: Vec<String>,
    evidence_snapshot_path: String,
    ownership_proof: Vec<String>,
    phase: StateLossRecoveryPhase,
    replacement: Option<DaemonStartResult>,
    replacement_startup_token: Option<String>,
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
) -> Result<Option<DaemonLeaselessRecoveryResult>> {
    let receipt_path = crate::core::paths::daemon_leaseless_recovery_receipt_file()?;
    let Some(mut receipt) = read_leaseless_recovery_receipt(&receipt_path)? else {
        return Ok(None);
    };
    let Some(state) = status.state.as_ref() else {
        return Ok(None);
    };
    if receipt.phase == StateLossRecoveryPhase::ReplacementStarting {
        if status.running
            && receipt.replacement_startup_token.as_deref() == Some(&state.startup_token)
        {
            receipt.phase = StateLossRecoveryPhase::ReplacementStarted;
            receipt.replacement = Some(DaemonStartResult {
                pid: state.pid,
                address: state.address.clone(),
                state_path: state.state_path.clone(),
                lease_id: state.lease_id.clone(),
            });
            write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
        } else if status.running {
            return Err(Error::validation_invalid_argument(
                "reconcile_leaseless_orphans",
                "lease-less recovery replay found a mismatched live daemon",
                None,
                None,
            ));
        } else {
            return Ok(None);
        }
    }
    if status.freshness.active_jobs != 0 || !status.fresh || !status.running {
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
    adopt_orphaned_lease_with_operations(
        lease_id,
        read_status,
        pid_is_running,
        try_acquire_daemon_owner_lock,
        || {
            let store = super::JobStore::open_without_reconciliation(
                crate::core::paths::daemon_jobs_file()?,
            )?;
            store.reconcile_dead_daemon_lease_jobs(lease_id)
        },
        || start_or_return_live_unlocked(addr),
    )
}

/// Recover one legacy durable job only after an operator supplies the exact
/// child PID and Linux start ticks recovered from trustworthy run evidence.
pub fn recover_missing_child_identity(
    expected_lease_id: &str,
    recorded_daemon_pid: u32,
    recorded_daemon_endpoint: &str,
    job_id: uuid::Uuid,
    child_pid: u32,
    child_starttime_ticks: u64,
) -> Result<crate::core::api_jobs::Job> {
    let endpoint = parse_recorded_daemon_endpoint(recorded_daemon_endpoint)?;
    let _operation_lock = acquire_daemon_operation_lock()?;
    let status = read_status()?;
    let state = status.state.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "legacy child recovery requires the persisted daemon lease record",
            Some(expected_lease_id.to_string()),
            None,
        )
    })?;
    if state.lease_id != expected_lease_id
        || state.pid != recorded_daemon_pid
        // Compare the persisted spelling as well as parsing the endpoint below.
        // Accepting a normalized equivalent would weaken the operator's exact
        // recovery proof against a changed daemon endpoint.
        || state.address != recorded_daemon_endpoint
    {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            "recorded daemon lease, PID, or endpoint does not match current daemon state",
            Some(expected_lease_id.to_string()),
            None,
        ));
    }
    if pid_is_running(recorded_daemon_pid) {
        return Err(Error::validation_invalid_argument(
            "recorded_daemon_pid",
            "recorded daemon PID is live; refusing legacy job recovery",
            Some(recorded_daemon_pid.to_string()),
            None,
        ));
    }
    probe_recorded_daemon_endpoint(endpoint)?;
    let _owner_lock = try_acquire_daemon_owner_lock()?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease_id",
            "daemon owner lock is held; refusing legacy job recovery",
            Some(expected_lease_id.to_string()),
            None,
        )
    })?;
    let store =
        super::JobStore::open_without_reconciliation(crate::core::paths::daemon_jobs_file()?)?;
    store.recover_missing_child_identity_with_linux_evidence(
        expected_lease_id,
        job_id,
        child_pid,
        child_starttime_ticks,
    )
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
    Reconcile: FnOnce() -> Result<crate::core::api_jobs::DaemonLeaseJobDiagnostics>,
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
/// Process and configured-listener probes are fail-closed because replacement
/// is safe only after ownership has been ruled out.
pub fn reconcile_leaseless_orphans(
    confirm_no_daemon_owner: bool,
    addr: &str,
) -> Result<DaemonLeaselessRecoveryResult> {
    if !confirm_no_daemon_owner {
        return Err(Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "lease-less recovery requires --confirm-no-daemon-owner",
            None,
            None,
        ));
    }
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    let status = read_status()?;
    if let Some(result) = replay_leaseless_recovery(&status)? {
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
    let receipt = LeaselessRecoveryReceipt {
        affected_job_ids: reconciled.reconciled_job_ids,
        affected_jobs: reconciled.affected_jobs,
        historical_lease_ids: reconciled.historical_lease_ids,
        evidence_snapshot_path: snapshot_path.display().to_string(),
        ownership_proof,
        phase: StateLossRecoveryPhase::Reconciled,
        replacement: None,
        replacement_startup_token: None,
    };
    let receipt_path = crate::core::paths::daemon_leaseless_recovery_receipt_file()?;
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    let mut receipt = receipt;
    let startup_token = uuid::Uuid::new_v4().to_string();
    receipt.phase = StateLossRecoveryPhase::ReplacementStarting;
    receipt.replacement_startup_token = Some(startup_token.clone());
    write_leaseless_recovery_receipt(&receipt_path, &receipt)?;
    let replacement = start_or_return_live_unlocked_with_startup_token(addr, &startup_token)?;
    let status = read_status()?;
    if !status.running
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

fn prove_no_daemon_owner(addr: &str) -> Result<Vec<String>> {
    // The owner lock proves no serving daemon owns this store. Refuse any
    // matching process or configured listener as additional ambiguous ownership.
    let candidates = daemon_process_candidates(&crate::core::paths::daemon_jobs_file()?)?;
    let parsed: SocketAddr = addr.parse().map_err(|_| {
        Error::validation_invalid_argument("addr", "invalid daemon address", None, None)
    })?;
    if !candidates_prove_no_owner(&candidates) {
        return Err(Error::validation_invalid_argument(
            "owner_probe",
            "a daemon serve process may own the configured durable store; refusing missing-lease recovery",
            None,
            Some(candidates.iter().map(|candidate| format!("pid {}: {:?} ({})", candidate.pid, candidate.ownership, candidate.cmdline)).collect()),
        ));
    }
    let process = if candidates.is_empty() {
        "supplemental process probe found no daemon serve candidates".to_string()
    } else {
        format!("supplemental process probe proved {} daemon candidate(s) unrelated to the configured durable store", candidates.len())
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
        process,
        listener,
    ])
}

fn candidates_prove_no_owner(candidates: &[DaemonProcessCandidate]) -> bool {
    candidates
        .iter()
        .all(|candidate| candidate.ownership == DaemonProcessOwnership::Unrelated)
}

fn pid_is_proven_dead(pid: u32, is_running: impl FnOnce(u32) -> bool) -> bool {
    !is_running(pid)
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
    let diagnostics = store.reconcile_dead_daemon_lease_jobs(expected_lease_id)?;
    if diagnostics.protected_count() > 0 {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "deferred dead-lease recovery because {} active child process(es) cannot be reattached: {}",
                diagnostics.protected_count(),
                diagnostics
                    .protected_job_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(expected_lease_id.to_string()),
            Some(vec![
                "Homeboy cannot collect an orphan child result; wait for it to exit, then retry exact recovery."
                    .to_string(),
            ]),
        ));
    }
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
    start_or_return_live_unlocked_with_startup_token(addr, &uuid::Uuid::new_v4().to_string())
}

fn start_or_return_live_unlocked_with_startup_token(
    addr: &str,
    startup_token: &str,
) -> Result<DaemonStartResult> {
    let _repaired_legacy_lease = repair_legacy_lease_for_start()?;
    reattach_exact_live_owner()?;
    start_or_return_live_with_operations(
        read_status,
        try_acquire_daemon_owner_lock,
        || stop_unlocked().map(|_| ()),
        || spawn_and_wait_for_lease(addr, startup_token),
    )
}

/// Restore the lease only for one process whose executable and explicit HOME
/// environment prove it owns this conventional durable store. The listener is
/// checked before writing anything; all other candidates remain fail-closed.
fn reattach_exact_live_owner() -> Result<()> {
    let state_path = crate::core::paths::daemon_state_file()?;
    if state_path.exists() {
        return Ok(());
    }
    let candidates = daemon_process_candidates(&crate::core::paths::daemon_jobs_file()?)?;
    let owners: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.ownership == DaemonProcessOwnership::Owning)
        .collect();
    if owners.len() != 1 {
        return Ok(());
    }
    let owner = &owners[0];
    let Some(endpoint) = owner.bind_endpoint.as_deref() else {
        return Ok(());
    };
    let Ok(address) = endpoint.parse::<SocketAddr>() else {
        return Ok(());
    };
    if address.port() == 0
        || TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err()
    {
        return Ok(());
    }
    if !pid_is_running(owner.pid) {
        return Ok(());
    }
    // Re-read the exact candidate immediately before persisting a lease so PID
    // reuse cannot turn a previously attributable process into an owner.
    let revalidated = daemon_process_candidates(&crate::core::paths::daemon_jobs_file()?)?
        .into_iter()
        .any(|candidate| {
            candidate.pid == owner.pid
                && candidate.ownership == DaemonProcessOwnership::Owning
                && candidate.cmdline == owner.cmdline
        });
    if !revalidated {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let state = super::DaemonState {
        schema: super::DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: uuid::Uuid::new_v4().to_string(),
        startup_token: "reattached".to_string(),
        address: endpoint.to_string(),
        pid: owner.pid,
        state_path: state_path.display().to_string(),
        started_at: now.clone(),
        last_seen_at: now,
        build_identity: crate::core::build_identity::current(),
        binary_sha256: super::current_binary_sha256()?,
        runtime_paths: super::capture_daemon_runtime_snapshot(),
    };
    super::write_lease(&state_path, &state)
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

fn spawn_and_wait_for_lease(addr: &str, startup_token: &str) -> Result<DaemonStartResult> {
    let exe = std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let mut command = Command::new(exe);
    command
        .args([
            "daemon",
            "supervise",
            "--addr",
            addr,
            "--startup-token",
            startup_token,
        ])
        .env(DAEMON_STARTUP_TOKEN_ENV, startup_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_from_launcher_session(&mut command);
    let child = command
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

/// Keep the daemon and its workload children alive when a transient launcher
/// connection, such as direct SSH, disconnects.
#[cfg(unix)]
fn detach_from_launcher_session(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: `pre_exec` runs in the child immediately before exec. `setsid`
    // only changes that child process's session and reports failure via errno.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach_from_launcher_session(_command: &mut Command) {}

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

#[cfg(test)]
mod termination_tests {
    use super::*;

    #[test]
    fn termination_output_is_bounded_and_redacted() {
        let output =
            bounded_redacted(format!("token=super-secret\n{}", "x".repeat(5_000)).as_bytes())
                .expect("output");
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("[truncated]"));
        assert!(output.len() < 4_200);
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_fixture_exit_preserves_exit_status_without_os_cause_inference() {
        let status = Command::new("sh")
            .args(["-c", "exit 23"])
            .status()
            .expect("fixture process");
        assert_eq!(exit_details(&status), (Some(23), None));
        let evidence = DaemonTerminationEvidence {
            classification: DaemonTerminationClassification::UnexpectedExit,
            observed_at: "2026-01-01T00:00:00Z".to_string(),
            lease_id: Some("lease".to_string()),
            pid: Some(1),
            binary_identity: Some("fixture".to_string()),
            active_jobs: 1,
            resource_evidence: "unavailable: fixture has no OS resource snapshot".to_string(),
            os_evidence: "unavailable: fixture has no OS evidence".to_string(),
            exit_code: Some(23),
            signal: None,
            stdout: None,
            stderr: Some("panic: fixture".to_string()),
            stop_requested: false,
        };
        assert_eq!(
            evidence.classification,
            DaemonTerminationClassification::UnexpectedExit
        );
        assert!(evidence.os_evidence.starts_with("unavailable:"));
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
#[path = "../../../tests/core/daemon/control_test.rs"]
mod control_test;
