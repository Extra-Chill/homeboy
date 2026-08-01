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

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::execution_contract::encode_uri_component;
use crate::process::{
    pid_has_ownership_token, pid_is_running, terminate_pid_with_sigterm_and_wait,
};

use super::{
    acquire_daemon_operation_lock, acquire_daemon_operation_lock_for_ensure, parse_bind_addr,
    read_status, repair_legacy_lease_for_start, stop_unlocked, try_acquire_daemon_owner_lock,
    DaemonExactOrphanRecoveryResult, DaemonLeaselessOrphanReconciliationResult,
    DaemonLeaselessRecoveryResult, DaemonOrphanAdoptionResult, DaemonProcessCandidate,
    DaemonProcessOwnership, DaemonStaleReasonCode, DaemonStartResult,
    DaemonTerminationClassification, DaemonTerminationEvidence, DAEMON_STARTUP_TOKEN_ENV,
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
        // The argument is the portable, exact ownership proof used when a
        // platform cannot inspect a process environment. Keep it aligned with
        // the persisted admission token for the lifetime of this daemon.
        .args([
            "daemon",
            "serve",
            "--addr",
            addr,
            "--startup-token",
            startup_token,
        ])
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
    supervise_child(child)
}

/// Own the post-spawn supervisor lifecycle. Kept separate so the real child
/// pipes and persisted evidence can be exercised without replacing the CLI.
fn supervise_child(mut child: std::process::Child) -> Result<()> {
    let pid = child.id();
    // A daemon can run indefinitely. Drain both pipes while it runs, retaining
    // only a diagnostic tail so supervisor RSS cannot grow with child output.
    let child_stdout = child.stdout.take().expect("piped stdout");
    let child_stderr = child.stderr.take().expect("piped stderr");
    let stdout = thread::spawn(move || bounded_redacted_reader(child_stdout));
    let stderr = thread::spawn(move || bounded_redacted_reader(child_stderr));
    let status = child.wait();
    let stdout = join_output_reader(stdout, "stdout");
    let stderr = join_output_reader(stderr, "stderr");
    let status = status.map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("wait for supervised daemon".to_string()),
        )
    })?;
    let state = super::validate_lease_file(&crate::paths::daemon_state_file()?)
        .ok()
        .and_then(|validation| validation.state);
    let prior = super::read_termination_evidence()?;
    let stop_requested = prior
        .as_ref()
        .is_some_and(|evidence| evidence.stop_requested && evidence.pid == Some(pid));
    let (exit_code, signal) = exit_details(&status);
    let evidence = DaemonTerminationEvidence {
        classification: if stop_requested { DaemonTerminationClassification::CleanStop } else { DaemonTerminationClassification::UnexpectedExit },
        observed_at: chrono::Utc::now().to_rfc3339(),
        lease_id: state.as_ref().map(|state| state.lease_id.clone()).or_else(|| prior.and_then(|evidence| evidence.lease_id)),
        pid: Some(pid),
        binary_identity: state.as_ref().map(|state| state.build_identity.display.clone()),
        active_jobs: super::JobStore::active_count_at_path(crate::paths::daemon_jobs_file()?)?,
        resource_evidence: "unavailable: launcher does not collect OS resource snapshots".to_string(),
        os_evidence: "unavailable: no OS evidence collected; exit status and signal are launcher observations only".to_string(),
        exit_code, signal,
        stdout, stderr, stop_requested,
    };
    super::write_termination_evidence(&evidence)
}

fn join_output_reader(reader: thread::JoinHandle<Option<String>>, stream: &str) -> Option<String> {
    reader
        .join()
        .unwrap_or_else(|_| Some(format!("[{stream} reader panicked after child reaped]")))
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
    let state_dir = cmdline
        .split_whitespace()
        .find_map(|part| part.strip_prefix(crate::paths::DAEMON_STATE_DIR_ENV))
        .and_then(|value| value.strip_prefix('='))
        .filter(|value| !value.trim().is_empty());
    let home = cmdline
        .split_whitespace()
        .find_map(|part| part.strip_prefix("HOME="));
    let durable_store_path = state_dir
        .map(|state_dir| Path::new(state_dir).join("jobs.json"))
        .or_else(|| home.map(|home| Path::new(home).join(".config/homeboy/daemon/jobs.json")));
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
///
/// PID death and control-plane loss are not operator assertions: both are
/// established here. `validate_state_loss_preconditions` requires an absent
/// state record, a `LeaseMissing` freshness code, an unreachable daemon, and a
/// non-running recorded PID, and then `probe_recorded_daemon_endpoint` must
/// fail to connect to the recorded endpoint. The former `--confirm-pid-dead`
/// and `--confirm-control-plane-lost` gates ran *before* all of that and so
/// could only reject correct operators.
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
        let jobs_path = crate::paths::daemon_jobs_file()?;
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
    Reconcile: FnOnce() -> Result<(PathBuf, crate::api_jobs::DaemonLeaseJobDiagnostics)>,
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
    Reconcile: FnOnce() -> Result<(PathBuf, crate::api_jobs::LeaselessOrphanJobDiagnostics)>,
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
        if let Some(state) = status.state.as_ref().filter(|_| status.running) {
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
            if !started.running
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
    start_or_return_live_unlocked(addr)
}

/// Return a live daemon under the lifecycle lock, or start one when its lease
/// is absent or its recorded PID is dead.
pub fn ensure_running(addr: &str) -> Result<DaemonStartResult> {
    ensure_running_with_wait(addr, Duration::from_secs(5))
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
    let _lock = acquire_daemon_operation_lock_for_ensure(Duration::from_secs(5))?;
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
                super::JobStore::open_without_reconciliation(crate::paths::daemon_jobs_file()?)?;
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
/// code, a non-running recorded PID, persisted unexpected-termination evidence
/// bound to this exact lease and PID, and a second liveness proof taken under
/// the owner lock. `--confirm-pid-dead` added nothing to that.
pub fn reconcile_dead_lease_orphans(
    lease_id: &str,
    job_ids: &[uuid::Uuid],
    confirm_workload_processes_absent: bool,
    addr: &str,
) -> Result<DaemonExactOrphanRecoveryResult> {
    if !confirm_workload_processes_absent {
        return Err(Error::validation_invalid_argument("confirm_workload_processes_absent", "exact dead-lease recovery requires --confirm-workload-processes-absent after inspecting workload processes", None, None));
    }
    parse_bind_addr(addr)?;
    let _lock = acquire_daemon_operation_lock()?;
    reconcile_dead_lease_orphans_with_operations(
        lease_id,
        job_ids,
        read_status,
        pid_is_running,
        try_acquire_daemon_owner_lock,
        || prove_no_daemon_owner(addr),
        |pid| {
            let store =
                super::JobStore::open_without_reconciliation(crate::paths::daemon_jobs_file()?)?;
            store.reconcile_exact_daemon_loss_jobs(lease_id, job_ids, pid)
        },
        || start_or_return_live_unlocked(addr),
    )
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
    _job_ids: &[uuid::Uuid],
    status: Status,
    pid_is_running: PidIsRunning,
    acquire_owner: AcquireOwner,
    prove_no_owner: ProveNoOwner,
    reconcile: Reconcile,
    start: Start,
) -> Result<DaemonExactOrphanRecoveryResult>
where
    Status: FnOnce() -> Result<super::DaemonStatus>,
    PidIsRunning: Fn(u32) -> bool,
    AcquireOwner: FnOnce() -> Result<Option<OwnerLock>>,
    ProveNoOwner: FnOnce() -> Result<Vec<String>>,
    Reconcile: FnOnce(u32) -> Result<crate::api_jobs::DaemonLeaseJobDiagnostics>,
    Start: FnOnce() -> Result<super::DaemonStartResult>,
{
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
    let termination = status.termination_evidence.ok_or_else(|| {
        Error::validation_invalid_argument(
            "termination_evidence",
            "exact dead-lease recovery requires persisted unexpected-termination evidence",
            Some(lease_id.to_string()),
            None,
        )
    })?;
    if termination.classification != DaemonTerminationClassification::UnexpectedExit
        || termination.stop_requested
        || termination.lease_id.as_deref() != Some(lease_id)
        || termination.pid != Some(state.pid)
    {
        return Err(Error::validation_invalid_argument(
            "termination_evidence",
            "persisted termination evidence does not prove this lease's unexpected daemon exit",
            Some(lease_id.to_string()),
            None,
        ));
    }
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
    let ownership_proof = prove_no_owner()?;
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

/// Recover one legacy durable job only after an operator supplies the exact
/// child PID and Linux start ticks recovered from trustworthy run evidence.
pub fn recover_missing_child_identity(
    expected_lease_id: &str,
    recorded_daemon_pid: u32,
    recorded_daemon_endpoint: &str,
    job_id: uuid::Uuid,
    child_pid: u32,
    child_starttime_ticks: u64,
) -> Result<crate::api_jobs::Job> {
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
    let store = super::JobStore::open_without_reconciliation(crate::paths::daemon_jobs_file()?)?;
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
///
/// Absence of a daemon owner is proven, not asserted: the daemon owner lock is
/// refused while any daemon is live or starting, and `prove_no_daemon_owner`
/// then fails closed on any related daemon process candidate or any reachable
/// listener at `addr`. The former `--confirm-no-daemon-owner` gate ran ahead of
/// both probes.
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
    let ownership_proof = prove_no_daemon_owner(addr)?;
    let jobs_path = crate::paths::daemon_jobs_file()?;
    let raw_store = read_job_store_bytes(&jobs_path)?;
    let snapshot_path = snapshot_job_store(&jobs_path, &raw_store)?;
    let store = super::JobStore::open_without_reconciliation_from_bytes(&jobs_path, &raw_store)?;
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
    let candidates = daemon_process_candidates(&crate::paths::daemon_jobs_file()?)?;
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
    let store = super::JobStore::open_without_reconciliation(crate::paths::daemon_jobs_file()?)?;
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
    refuse_unleased_process_conflict()?;
    start_or_return_live_with_operations(
        read_status,
        try_acquire_daemon_owner_lock,
        || stop_unlocked().map(|_| ()),
        || spawn_and_wait_for_lease(addr, startup_token),
    )
}

/// A missing lease cannot authorize replacement when another foreground daemon
/// may still own this store. Starting a child in this state only delays the
/// diagnosis until that child fails to acquire `owner.lock`; return the typed
/// evidence immediately and never signal a process we cannot prove we own.
fn refuse_unleased_process_conflict() -> Result<()> {
    let state_path = crate::paths::daemon_state_file()?;
    let candidates = daemon_process_candidates(&crate::paths::daemon_jobs_file()?)?
        .into_iter()
        .filter(|candidate| {
            matches!(
                candidate.ownership,
                DaemonProcessOwnership::Owning | DaemonProcessOwnership::Ambiguous
            )
        })
        .collect::<Vec<_>>();
    refuse_unleased_process_conflict_with_candidates(&state_path, candidates)
}

/// A stale lease does not authorize replacement when foreground process
/// evidence is still ambiguous. Bound the returned evidence so status and
/// recovery errors remain safe to render in control-plane callers.
fn refuse_unleased_process_conflict_with_candidates(
    state_path: &Path,
    candidates: Vec<DaemonProcessCandidate>,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    const EVIDENCE_LIMIT: usize = 8;
    let candidate_count = candidates.len();
    let candidates = candidates
        .into_iter()
        .take(EVIDENCE_LIMIT)
        .collect::<Vec<_>>();

    let mut error = Error::internal_unexpected(
        "daemon lease is absent or stale while foreground daemon candidates remain live; refusing replacement without an authoritative lease",
    );
    error.details = serde_json::json!({
        "classification": "daemon_unleased_process_conflict",
        "state_path": state_path,
        "candidates": candidates,
        "candidate_count": candidate_count,
        "candidates_truncated": candidate_count > EVIDENCE_LIMIT,
        "safe_next_action": "Run `homeboy daemon status` and reconcile only a daemon whose PID, binary identity, durable-store path, and startup token are all attributable to this state directory.",
    });
    Err(error.with_hint(
        "No process was terminated. Preserve active jobs and inspect the reported candidates before an explicit lease-bound stop or recovery.".to_string(),
    ))
}

/// Restore the lease only for one process whose executable and explicit HOME
/// environment prove it owns this conventional durable store. The listener is
/// checked before writing anything; all other candidates remain fail-closed.
fn reattach_exact_live_owner() -> Result<()> {
    let state_path = crate::paths::daemon_state_file()?;
    if state_path.exists() {
        return Ok(());
    }
    let candidates = daemon_process_candidates(&crate::paths::daemon_jobs_file()?)?;
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
    let revalidated = daemon_process_candidates(&crate::paths::daemon_jobs_file()?)?
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
        build_identity: crate::build_identity::current(),
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

const STARTUP_LEASE_OBSERVATIONS: usize = 100;
const STARTUP_LEASE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupLeaseObservation {
    observed_pid: Option<u32>,
    observed_lease_id: Option<String>,
    observed_token: Option<String>,
}

/// A launch token is an attempt identity, never a general daemon readiness
/// signal. The spawned `supervise` launcher and its `serve` child have distinct
/// PIDs, so the live child lease is identified by its unique token.
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
    };
    for attempt in 0..=observations {
        let status = read_status()?;
        if let Some(state) = status.state {
            if status.running && state.startup_token == startup_token {
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

fn cleanup_startup_attempt(pid: u32, startup_token: &str) -> Result<Vec<String>> {
    let mut cleanup = Vec::new();
    let state_path = crate::paths::daemon_state_file()?;
    let status = read_status()?;
    if let Some(state) = status
        .state
        .filter(|state| state.startup_token == startup_token)
    {
        let identity = super::DaemonLeaseIdentity::from_state(&state);
        if !pid_is_running(state.pid) {
            super::remove_lease_if_identity_matches(&state_path, &identity)?;
            cleanup.push(format!("removed stale token lease for pid {}", state.pid));
        } else if pid_has_ownership_token(state.pid, DAEMON_STARTUP_TOKEN_ENV, startup_token)? {
            terminate_pid_with_sigterm_and_wait(state.pid, super::FORCE_STOP_WAIT)?;
            super::remove_lease_if_identity_matches(&state_path, &identity)?;
            cleanup.push(format!("terminated token-owned daemon pid {}", state.pid));
        } else {
            cleanup.push(format!(
                "retained lease for pid {} because its token ownership could not be proven",
                state.pid
            ));
        }
    }
    if pid_is_running(pid) && pid_has_ownership_token(pid, DAEMON_STARTUP_TOKEN_ENV, startup_token)?
    {
        terminate_pid_with_sigterm_and_wait(pid, super::FORCE_STOP_WAIT)?;
        cleanup.push(format!("terminated token-owned launcher pid {pid}"));
    }
    Ok(cleanup)
}

fn startup_timeout_error(
    pid: u32,
    startup_token: &str,
    observation: StartupLeaseObservation,
    cleanup: Vec<String>,
) -> Error {
    let mut error = Error::internal_unexpected(format!(
        "daemon process {pid} did not publish its isolated startup token before timeout"
    ))
    .with_hint("The daemon startup was not accepted because the expected token and PID did not match. Retry the Lab Cook; provider budget was not consumed.".to_string());
    error.details = serde_json::json!({
        "classification": "terminal_pre_provider_startup",
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

fn spawn_and_wait_for_lease(addr: &str, startup_token: &str) -> Result<DaemonStartResult> {
    spawn_and_wait_for_lease_attempt(addr, startup_token, true, Vec::new())
}

fn spawn_and_wait_for_lease_attempt(
    addr: &str,
    startup_token: &str,
    allow_retry: bool,
    mut cleanup_evidence: Vec<String>,
) -> Result<DaemonStartResult> {
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

    match observe_startup_lease(
        pid,
        startup_token,
        STARTUP_LEASE_OBSERVATIONS,
        read_status,
        || thread::sleep(STARTUP_LEASE_POLL),
    )? {
        Ok(result) => Ok(result),
        Err(observation) => {
            let cleanup = cleanup_startup_attempt(pid, startup_token)?;
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
                    false,
                    cleanup_evidence,
                );
            }
            Err(startup_timeout_error(
                pid,
                startup_token,
                observation,
                cleanup_evidence,
            ))
        }
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

    #[cfg(unix)]
    #[test]
    fn supervise_child_persists_bounded_redacted_concurrent_pipe_output() {
        crate::test_support::with_isolated_home(|_| {
            let child = Command::new("sh")
                .args([
                    "-c",
                    "(i=0; while [ $i -lt 4096 ]; do printf 'stdout-%s token=stdout-secret\\n' \"$i\"; i=$((i + 1)); done) & (i=0; while [ $i -lt 4096 ]; do printf 'stderr-%s token=stderr-secret\\n' \"$i\" >&2; i=$((i + 1)); done) & wait",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("concurrent output fixture");

            supervise_child(child).expect("supervise fixture");

            let evidence = super::super::read_termination_evidence()
                .expect("read evidence")
                .expect("termination evidence");
            for output in [evidence.stdout, evidence.stderr] {
                let output = output.expect("stream evidence");
                assert!(output.len() < 4_200);
                assert!(output.contains("token=[REDACTED]"));
                assert!(!output.contains("secret"));
            }
        });
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
#[path = "../../../../tests/core/daemon/control_test.rs"]
mod control_test;
