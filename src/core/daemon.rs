use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};
use uuid::Uuid;

use crate::command_contract::RunnerWorkload;
use crate::core::api_jobs::{JobStatus, JobStore, RunnerJobLifecycleMetadata};
use crate::core::build_identity;
use crate::core::error::{Error, RemoteCommandFailedDetails, Result, TargetDetails};
use crate::core::http_api::{self, AnalysisJobRunner, HttpMethod, UnsupportedAnalysisJobRunner};
use crate::core::paths;
use crate::core::process::pid_is_running;
use crate::core::runner::{
    execute_runner_process_until_cancelled_with_progress, prepare_daemon_local_process,
    BrokerScope, Runner, RunnerProcessRequest,
};
use crate::core::runner_execution_envelope::PathMaterializationPlan;
use crate::core::secret_env_plan::SecretEnvPlan;
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::upgrade::VERSION;

mod artifact_download;
mod broker_config;
mod completion_tracker;
mod control;
mod patch_capture;
mod remote_runner;
pub use artifact_download::ArtifactDownload;
pub use broker_config::{render_broker_config, BrokerConfig, BrokerConfigOptions, ServiceIdentity};
pub use control::{
    adopt_orphaned_lease, artifact_content_url, ensure_running, fetch_artifact_to_path,
    reconcile_leaseless_orphans, start_background, ArtifactFetchOutcome,
};
use patch_capture::{capture_baseline, capture_patch_report};

pub const DEFAULT_ADDR: &str = "127.0.0.1:0";

static DAEMON_JOB_STORE: OnceLock<JobStore> = OnceLock::new();
static DAEMON_RUNTIME_SNAPSHOT: OnceLock<DaemonRuntimeSnapshot> = OnceLock::new();

const DAEMON_LEASE_SCHEMA: &str = "homeboy.daemon.session_lease.v1";
const DAEMON_STARTUP_TOKEN_ENV: &str = "HOMEBOY_DAEMON_STARTUP_TOKEN";
const RUNTIME_PATH_FILE_LIMIT: usize = 2_000;
const RUNTIME_PATH_SUFFIXES: &[&str] = &["_COMPONENT_PATH", "_PROVIDER_PATH", "_RUNTIME_PATH"];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonState {
    /// Older writers could omit this field. Retain the remaining identity as
    /// stale evidence instead of losing the lease and PID during deserialization.
    #[serde(default)]
    pub schema: String,
    pub lease_id: String,
    #[serde(default)]
    pub startup_token: String,
    pub address: String,
    pub pid: u32,
    pub state_path: String,
    pub started_at: String,
    pub last_seen_at: String,
    pub build_identity: build_identity::BuildIdentity,
    pub binary_sha256: Option<String>,
    pub runtime_paths: DaemonRuntimeSnapshot,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    pub fresh: bool,
    pub reachable: bool,
    pub freshness: DaemonFreshnessReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    pub state_path: String,
    /// Identity of the lease and durable queue observed by status callers.
    pub state_identity: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonStaleReasonCode {
    LeaseMissing,
    LeaseCorrupt,
    LeaseSchemaMismatch,
    PidDead,
    BuildIdentityMismatch,
    BinaryHashMismatch,
    VersionMismatch,
    RuntimePathsDrift,
    TransportUnreachable,
}

/// Whether the daemon lease data is sufficient for an explicit recovery action.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonRecoveryEvidence {
    ProvenDead,
    Unavailable,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonRepairStep {
    pub code: String,
    pub command: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonFreshnessReport {
    pub fresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason_code: Option<DaemonStaleReasonCode>,
    pub restartable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_evidence: Option<DaemonRecoveryEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership_evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adoption_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_hash: Option<String>,
    /// Version observed from the reachable daemon endpoint, rather than inferred
    /// from the selected runner executable or a persisted controller session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    /// Build identity observed from the reachable daemon endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_paths: Option<DaemonRuntimeSnapshot>,
    pub active_jobs: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repair_plan: Vec<DaemonRepairStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonStartResult {
    pub pid: u32,
    pub address: String,
    pub state_path: String,
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonOrphanAdoptionResult {
    pub adopted_lease_id: String,
    pub dead_pid: u32,
    pub active_jobs_terminalized: usize,
    pub retry_guidance: String,
    pub replacement: DaemonStartResult,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonLeaselessRecoveryResult {
    pub affected_job_ids: Vec<Uuid>,
    pub affected_job_count: usize,
    #[serde(default)]
    pub affected_jobs: Vec<crate::core::api_jobs::LeaselessOrphanAffectedJob>,
    #[serde(default)]
    pub historical_lease_ids: Vec<String>,
    pub evidence_snapshot_path: String,
    pub ownership_proof: Vec<String>,
    pub retry_guidance: String,
    pub replacement: DaemonStartResult,
}

/// Compatibility result for the local reconciliation helper used by lifecycle
/// tests. The CLI uses `DaemonLeaselessRecoveryResult`, which includes the
/// structured ownership evidence returned by remote recovery.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonLeaselessOrphanReconciliationResult {
    pub snapshot_path: String,
    pub affected_job_ids: Vec<String>,
    pub affected_job_count: usize,
    pub no_owner_proof: Vec<String>,
    pub retry_guidance: String,
    pub replacement: DaemonStartResult,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStopResult {
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub state_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonLeaseIdentity {
    lease_id: String,
    startup_token: String,
}

impl DaemonLeaseIdentity {
    fn from_state(state: &DaemonState) -> Self {
        Self {
            lease_id: state.lease_id.clone(),
            startup_token: state.startup_token.clone(),
        }
    }

    fn matches(&self, state: &DaemonState) -> bool {
        self.lease_id == state.lease_id && self.startup_token == state.startup_token
    }
}

#[derive(Debug)]
pub(super) struct DaemonOperationLock {
    path: PathBuf,
}

/// Held by the serving daemon for its entire lifetime. Unlike the short
/// lifecycle marker, this is an advisory OS lock so a crashed process releases
/// ownership without requiring unsafe stale-file deletion.
#[derive(Debug)]
pub(super) struct DaemonOwnerLock {
    file: File,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonRuntimeSnapshot {
    pub loaded_at: String,
    pub paths: Vec<DaemonRuntimePathSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonRuntimePathSnapshot {
    pub env: String,
    pub path: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonLeaseValidation {
    state: Option<DaemonState>,
    running: bool,
    fresh: bool,
    reachable: bool,
    stale_reason: Option<String>,
    stale_reason_code: Option<DaemonStaleReasonCode>,
    invalid_pid: Option<u32>,
}

/// The only pre-schema lease shape that can be repaired during daemon start.
/// Keeping this separate from `DaemonState` prevents legacy data from becoming
/// a supported format for normal reads, status, or stop operations.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDaemonState {
    address: String,
    pid: u32,
    state_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub body: serde_json::Value,
    pub artifact: Option<ArtifactDownload>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExecRequest {
    runner_id: String,
    #[serde(default)]
    runner: Option<Runner>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    command: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    secret_env_names: Vec<String>,
    #[serde(default)]
    secret_env_plan: SecretEnvPlan,
    #[serde(default)]
    capture_patch: bool,
    #[serde(default)]
    raw_exec: bool,
    #[serde(default)]
    source_snapshot: Option<SourceSnapshot>,
    #[serde(default)]
    path_materialization_plan: Option<PathMaterializationPlan>,
    #[serde(default)]
    require_paths: Vec<String>,
    #[serde(default)]
    runner_workload: Option<RunnerWorkload>,
    #[serde(default)]
    lifecycle: Option<RunnerJobLifecycleMetadata>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct FilePathRequest {
    runner_id: String,
    path: String,
    #[serde(default)]
    workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileUploadRequest {
    runner_id: String,
    path: String,
    #[serde(default)]
    workspace_root: Option<String>,
    content_base64: String,
}

pub fn parse_bind_addr(addr: &str) -> Result<SocketAddr> {
    let parsed: SocketAddr = addr.parse().map_err(|e| {
        Error::validation_invalid_argument(
            "addr",
            format!("Invalid daemon bind address `{}`: {}", addr, e),
            Some(addr.to_string()),
            Some(vec!["Use a host:port value like 127.0.0.1:0".to_string()]),
        )
    })?;

    if !parsed.ip().is_loopback() {
        return Err(Error::validation_invalid_argument(
            "addr",
            "Daemon MVP only accepts loopback bind addresses",
            Some(addr.to_string()),
            Some(vec!["Use 127.0.0.1:<port> or [::1]:<port>".to_string()]),
        ));
    }

    Ok(parsed)
}

fn state_path() -> Result<PathBuf> {
    paths::daemon_state_file()
}

pub(super) fn repair_legacy_lease_for_start() -> Result<bool> {
    let path = state_path()?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("read {}", path.display())),
            ))
        }
    };

    // Current leases continue through the regular lifecycle without a legacy
    // compatibility branch.
    if serde_json::from_str::<DaemonState>(&content).is_ok() {
        return Ok(false);
    }

    let legacy: LegacyDaemonState = serde_json::from_str(&content).map_err(|error| {
        legacy_lease_repair_error(
            &path,
            format!("state is neither the current schema nor the known pre-schema shape: {error}"),
        )
    })?;
    if legacy.state_path != path.display().to_string() {
        return Err(legacy_lease_repair_error(
            &path,
            format!(
                "legacy state_path `{}` does not match the expected state path",
                legacy.state_path
            ),
        ));
    }
    let endpoint: SocketAddr = legacy.address.parse().map_err(|error| {
        legacy_lease_repair_error(
            &path,
            format!("legacy address `{}` is invalid: {error}", legacy.address),
        )
    })?;
    if pid_is_running(legacy.pid) {
        return Err(legacy_lease_repair_error(
            &path,
            format!("legacy daemon pid {} is still running", legacy.pid),
        ));
    }
    if TcpStream::connect_timeout(&endpoint, Duration::from_millis(200)).is_ok() {
        return Err(legacy_lease_repair_error(
            &path,
            format!("legacy daemon endpoint {} is still reachable", endpoint),
        ));
    }

    let evidence_path = path.with_file_name(format!(
        "{}.legacy-lease-{}.json",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state.json"),
        Uuid::new_v4()
    ));
    fs::rename(&path, &evidence_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "archive legacy daemon lease {} to {}",
                path.display(),
                evidence_path.display()
            )),
        )
    })?;
    Ok(true)
}

fn legacy_lease_repair_error(path: &Path, problem: impl Into<String>) -> Error {
    Error::validation_invalid_argument(
        "daemon_lease",
        format!(
            "daemon lease at {} cannot be safely repaired: {}",
            path.display(),
            problem.into()
        ),
        Some(path.display().to_string()),
        Some(vec![format!(
            "Inspect {} and verify its recorded PID and endpoint before retrying `homeboy daemon start`",
            path.display()
        )]),
    )
}

pub fn read_status() -> Result<DaemonStatus> {
    let path = state_path()?;
    let state_path = path.display().to_string();
    let state_identity = daemon_state_identity(&path, &paths::daemon_jobs_file()?)?;
    let validation = validate_lease_file(&path)?;
    let active_jobs = JobStore::active_count_at_path(paths::daemon_jobs_file()?)?;

    Ok(DaemonStatus {
        running: validation.running && validation.fresh && validation.reachable,
        fresh: validation.fresh,
        reachable: validation.reachable,
        freshness: freshness_report_from_validation(&validation, active_jobs),
        stale_reason: validation.stale_reason,
        state: validation.state,
        state_path,
        state_identity,
    })
}

fn daemon_state_identity(state_path: &Path, jobs_path: &Path) -> Result<String> {
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

fn validate_lease_file(path: &Path) -> Result<DaemonLeaseValidation> {
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

fn stale_lease(
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

fn freshness_report_from_validation(
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
        repair_plan,
    }
}

fn runtime_snapshot_stale_reason(snapshot: &DaemonRuntimeSnapshot) -> Option<String> {
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

pub fn stop() -> Result<DaemonStopResult> {
    let _lock = acquire_daemon_operation_lock()?;
    stop_unlocked()
}

fn stop_unlocked() -> Result<DaemonStopResult> {
    let path = state_path()?;
    let state_path_display = path.display().to_string();
    let validation = validate_lease_file(&path)?;
    if validation.invalid_pid.is_some()
        || (validation.state.is_none() && validation.stale_reason.is_some() && path.exists())
    {
        return Err(corrupt_daemon_lease_error(&path, validation.stale_reason));
    }

    let Some(state) = validation.state.as_ref() else {
        return Ok(DaemonStopResult {
            stopped: false,
            pid: None,
            state_path: state_path_display,
        });
    };

    if !validation.fresh || !validation.running {
        if !validation.running && path.exists() {
            remove_lease_if_identity_matches(&path, &DaemonLeaseIdentity::from_state(state))?;
        }
        return Ok(DaemonStopResult {
            stopped: false,
            pid: Some(state.pid),
            state_path: state_path_display,
        });
    }

    if state.startup_token.is_empty() {
        return Err(Error::validation_invalid_argument(
            "daemon_lease",
            format!(
                "daemon lease at {} does not contain a startup token; refusing to signal pid {}",
                path.display(),
                state.pid
            ),
            Some(path.display().to_string()),
            Some(vec![
                "Start the daemon with `homeboy daemon start` so lifecycle ownership is tokenized"
                    .to_string(),
                format!(
                    "If this lease is stale, remove {} manually after verifying the pid",
                    path.display()
                ),
            ]),
        ));
    }

    let identity = DaemonLeaseIdentity::from_state(state);
    let pid = state.pid;
    let current_state = read_lease_if_identity_matches(&path, &identity)?;
    if current_state.pid != pid {
        return Err(Error::internal_unexpected(format!(
            "daemon lease changed pid from {} to {}; refusing to signal",
            pid, current_state.pid
        )));
    }

    let stopped = if pid_is_running(pid) {
        terminate_pid(pid)?;
        true
    } else {
        false
    };

    remove_lease_if_identity_matches(&path, &identity)?;

    Ok(DaemonStopResult {
        stopped,
        pid: Some(pid),
        state_path: state_path_display,
    })
}

pub fn serve(addr: SocketAddr) -> Result<DaemonState> {
    serve_with_analysis_runner(addr, UnsupportedAnalysisJobRunner)
}

pub fn serve_with_analysis_runner<R>(addr: SocketAddr, analysis_runner: R) -> Result<DaemonState>
where
    R: AnalysisJobRunner,
{
    let owner_lock = acquire_daemon_owner_lock()?;
    let listener = TcpListener::bind(addr)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("bind daemon to {}", addr))))?;
    serve_listener_with_analysis_runner_locked(listener, analysis_runner, owner_lock)
}

#[cfg(test)]
pub(crate) fn serve_listener(listener: TcpListener) -> Result<DaemonState> {
    let owner_lock = acquire_daemon_owner_lock()?;
    serve_listener_with_analysis_runner_locked(listener, UnsupportedAnalysisJobRunner, owner_lock)
}

fn serve_listener_with_analysis_runner_locked<R>(
    listener: TcpListener,
    analysis_runner: R,
    _owner_lock: DaemonOwnerLock,
) -> Result<DaemonState>
where
    R: AnalysisJobRunner,
{
    let local_addr = listener.local_addr().map_err(|e| {
        Error::internal_io(e.to_string(), Some("read daemon local address".to_string()))
    })?;
    let state = write_state(local_addr)?;
    let job_store = JobStore::open_without_reconciliation(paths::daemon_jobs_file()?)
        .map(|store| store.with_daemon_lease(state.lease_id.clone()))?;
    let _ = daemon_runtime_snapshot();
    let loopback_bind = local_addr.ip().is_loopback();

    spawn_completion_notifier();

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ =
                    handle_connection(stream, &job_store, analysis_runner.clone(), loopback_bind);
            }
            Err(err) => {
                return Err(Error::internal_io(
                    err.to_string(),
                    Some("accept daemon connection".to_string()),
                ));
            }
        }
    }

    Ok(state)
}

/// Environment variable overriding the completion-notifier poll interval in
/// seconds. Defaults to [`COMPLETION_NOTIFY_DEFAULT_INTERVAL_SECS`].
const COMPLETION_NOTIFY_INTERVAL_ENV: &str = "HOMEBOY_DAEMON_NOTIFY_INTERVAL_SECS";
const COMPLETION_NOTIFY_DEFAULT_INTERVAL_SECS: u64 = 5;

/// Spawn the background thread that watches in-flight runs and fires a local
/// notification when one completes, so a detached/offloaded run never becomes a
/// ghost.
///
/// Delivery is route-driven. Route-less completions are considered only when an
/// explicit operations default transport is configured.
fn spawn_completion_notifier() {
    let interval = std::env::var(COMPLETION_NOTIFY_INTERVAL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(COMPLETION_NOTIFY_DEFAULT_INTERVAL_SECS);
    std::thread::spawn(move || completion_notify_loop(std::time::Duration::from_secs(interval)));
}

/// Poll the observation store for in-flight runs and notify on completion.
///
/// Each pass refreshes mirrored runner evidence for currently-running records
/// (so offloaded runs advance to their terminal state locally), re-reads the
/// running set, and pings for any run that left it since the previous pass.
fn completion_notify_loop(interval: std::time::Duration) {
    use crate::core::observation::ObservationStore;

    let mut tracker = completion_tracker::CompletionTracker::default();
    loop {
        if let Ok(store) = ObservationStore::open_initialized() {
            let running = list_running_run_ids(&store);
            for run_id in &running {
                crate::core::observation::runs_service::refresh_mirrored_daemon_evidence_best_effort(
                    run_id,
                );
            }
            let running_after = list_running_run_ids(&store);
            for completed_id in tracker.observe(running_after) {
                let run = store.get_run(&completed_id).ok().flatten();
                let status = run
                    .as_ref()
                    .map(|run| run.status.as_str())
                    .unwrap_or("unknown");
                let route = run.as_ref().and_then(|run| {
                    crate::core::notification_route::NotificationRoute::from_metadata(
                        &run.metadata_json,
                    )
                });
                let event = crate::core::notify::NotifyEvent::run_completed_with_route(
                    &completed_id,
                    status,
                    route.as_ref(),
                );
                let _ = crate::core::notify::dispatch(&event);
            }
        }
        std::thread::sleep(interval);
    }
}

fn list_running_run_ids(store: &crate::core::observation::ObservationStore) -> Vec<String> {
    store
        .list_runs(crate::core::observation::RunListFilter {
            status: Some(
                crate::core::observation::RunStatus::Running
                    .as_str()
                    .to_string(),
            ),
            limit: Some(1000),
            ..Default::default()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|run| run.id)
        .collect()
}

pub fn route(method: &str, path: &str) -> HttpResponse {
    route_with_job_store(method, path, daemon_job_store())
}

fn route_with_job_store(method: &str, path: &str, job_store: &JobStore) -> HttpResponse {
    route_with_job_store_and_body(method, path, None, job_store)
}

fn route_with_job_store_and_body(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
) -> HttpResponse {
    route_with_job_store_and_body_and_runner(
        method,
        path,
        body,
        job_store,
        UnsupportedAnalysisJobRunner,
    )
}

fn route_with_job_store_and_body_and_runner<R>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: R,
) -> HttpResponse
where
    R: AnalysisJobRunner,
{
    // In-process dispatch (CLI internals, tests) is already inside the trust
    // boundary; only the network entry point (`handle_connection`) authenticates
    // broker traffic against the on-disk auth store.
    route_with_job_store_and_body_and_runner_and_auth(
        method,
        path,
        body,
        job_store,
        analysis_runner,
        remote_runner::BrokerAuthContext::trusted_local(),
    )
}

fn route_with_job_store_and_body_and_runner_and_auth<R>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: R,
    broker_auth: remote_runner::BrokerAuthContext,
) -> HttpResponse
where
    R: AnalysisJobRunner,
{
    match (method, path) {
        ("GET", "/health") => HttpResponse {
            status_code: 200,
            body: json!({
                "status": "ok",
                "version": VERSION,
                "build_identity": build_identity::current(),
                "lease": heartbeat_lease().ok(),
                "freshness": daemon_freshness_report(job_store).ok(),
            }),
            artifact: None,
        },
        ("GET", "/version") => HttpResponse {
            status_code: 200,
            body: json!({
                "version": VERSION,
                "build_identity": build_identity::current(),
                "runtime_paths": daemon_runtime_paths_body(),
                "lease": heartbeat_lease().ok(),
                "freshness": daemon_freshness_report(job_store).ok(),
            }),
            artifact: None,
        },
        ("GET", "/config/paths") => match config_paths_body() {
            Ok(body) => HttpResponse {
                status_code: 200,
                body,
                artifact: None,
            },
            Err(err) => error_response(500, err),
        },
        ("POST", "/health") | ("POST", "/version") | ("POST", "/config/paths") => HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
            artifact: None,
        },
        ("POST", "/exec") => match enqueue_exec_job(body, job_store) {
            Ok(body) => daemon_endpoint_response("jobs.exec", body),
            Err(err) => error_response(400, err),
        },
        ("POST", "/files/mkdir") => match create_runner_file_directory(body, &broker_auth) {
            Ok(body) => daemon_endpoint_response("files.mkdir", body),
            Err(err) => remote_runner::auth_or_bad_request(err),
        },
        ("POST", "/files/upload") => match upload_runner_file(body, &broker_auth) {
            Ok(body) => daemon_endpoint_response("files.upload", body),
            Err(err) => remote_runner::auth_or_bad_request(err),
        },
        ("POST", "/files/download") => match download_runner_file(body, &broker_auth) {
            Ok(body) => daemon_endpoint_response("files.download", body),
            Err(err) => remote_runner::auth_or_bad_request(err),
        },
        ("GET", "/exec") => HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
            artifact: None,
        },
        ("GET", "/files/mkdir") | ("GET", "/files/upload") | ("GET", "/files/download") => {
            method_not_allowed()
        }
        ("POST", "/runner/sessions")
        | ("POST", "/runner/jobs")
        | ("POST", "/runner/jobs/reconcile")
        | ("POST", "/runner/jobs/claim") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        ("GET", "/runner/sessions")
        | ("GET", "/runner/jobs")
        | ("GET", "/runner/jobs/reconcile")
        | ("GET", "/runner/jobs/claim") => method_not_allowed(),
        ("GET", path) if path.starts_with("/runner/jobs/") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        ("POST", path) if path.starts_with("/runner/jobs/") => {
            remote_runner::route(method, path, body, job_store, &broker_auth)
        }
        _ => route_read_only_api(method, path, body, job_store, analysis_runner),
    }
}

fn daemon_freshness_report(job_store: &JobStore) -> Result<DaemonFreshnessReport> {
    let path = state_path()?;
    let validation = validate_lease_file(&path)?;
    let active_jobs = job_store
        .list()
        .into_iter()
        .filter(|job| matches!(job.status, JobStatus::Queued | JobStatus::Running))
        .count();
    Ok(freshness_report_from_validation(&validation, active_jobs))
}

fn create_runner_file_directory(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FilePathRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file mkdir request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    fs::create_dir_all(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("create {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
    }))
}

fn upload_runner_file(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FileUploadRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file upload request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let content = base64::engine::general_purpose::STANDARD
        .decode(&request.content_base64)
        .map_err(|err| {
            Error::validation_invalid_argument(
                "content_base64",
                format!("runner file upload content is not valid base64: {err}"),
                None,
                None,
            )
        })?;
    fs::write(&path, &content).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
        "size_bytes": content.len(),
    }))
}

fn download_runner_file(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FilePathRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file download request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    let content = fs::read(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
        "size_bytes": content.len(),
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    }))
}

fn resolve_runner_workspace_path(
    runner_id: &str,
    requested_path: &str,
    request_workspace_root: Option<&str>,
) -> Result<PathBuf> {
    let loaded_runner;
    let workspace_root = match request_workspace_root.filter(|root| !root.trim().is_empty()) {
        Some(root) => root,
        None => {
            loaded_runner = crate::core::runner::load(runner_id)?;
            loaded_runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace_root",
                    format!("runner `{runner_id}` file API requires workspace_root"),
                    Some(runner_id.to_string()),
                    Some(vec![
                        "Configure the runner workspace_root before using daemon file transfer."
                            .to_string(),
                    ]),
                )
            })?
        }
    };
    let root = fs::canonicalize(workspace_root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "canonicalize runner workspace_root {workspace_root}"
            )),
        )
    })?;
    let requested = Path::new(requested_path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let normalized = canonicalize_existing_prefix(&normalize_path(&candidate));
    if !normalized.starts_with(&root) {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner file path must stay inside the runner workspace_root",
            Some(requested_path.to_string()),
            Some(vec![format!(
                "Runner `{runner_id}` workspace_root is {}.",
                root.display()
            )]),
        ));
    }
    Ok(normalized)
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }

    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(current) {
            let mut resolved = canonical;
            for component in missing.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(file_name) = current.file_name() else {
            return path.to_path_buf();
        };
        missing.push(file_name.to_os_string());
        let Some(parent) = current.parent() else {
            return path.to_path_buf();
        };
        current = parent;
    }
}

pub(super) fn daemon_endpoint_response(endpoint: &str, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status_code: 200,
        body: json!({
            "status": 200,
            "endpoint": endpoint,
            "body": body,
        }),
        artifact: None,
    }
}

fn method_not_allowed() -> HttpResponse {
    HttpResponse {
        status_code: 405,
        body: json!({ "error": "method_not_allowed" }),
        artifact: None,
    }
}

fn enqueue_exec_job(
    body: Option<serde_json::Value>,
    job_store: &JobStore,
) -> Result<serde_json::Value> {
    let mut request: ExecRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::validation_invalid_argument(
                "body",
                format!("invalid exec request body: {err}"),
                None,
                None,
            )
        })?;
    let mut base_plan = request
        .runner_workload
        .as_ref()
        .map(|workload| workload.required_secrets.secret_env_plan.clone())
        .filter(|plan| *plan != SecretEnvPlan::default());
    if request.secret_env_plan != SecretEnvPlan::default() {
        if let Some(plan) = base_plan.as_mut() {
            plan.merge_from(request.secret_env_plan.clone());
        } else {
            base_plan = Some(request.secret_env_plan.clone());
        }
    }
    let secret_env_plan = crate::core::runner::runner_exec_secret_env_plan(
        &request.command,
        None,
        &request.secret_env_names,
        &request.env,
        base_plan,
    );
    request.secret_env_names = secret_env_plan.secret_env_names();
    request.secret_env_plan = secret_env_plan.clone();
    crate::core::runner::workload::validate_runner_workload_dispatch(
        request.runner_workload.as_ref(),
        &request.runner_id,
        request.cwd.as_deref(),
        &request.command,
        &secret_env_plan,
        request.capture_patch,
    )?;
    let plan = prepare_daemon_local_process(RunnerProcessRequest {
        runner_id: request.runner_id,
        runner: request.runner,
        cwd: request.cwd,
        project_id: request.project_id,
        command: request.command,
        env: request.env,
        secret_env_names: request.secret_env_names,
        secret_env_plan: Some(secret_env_plan),
        capture_patch: request.capture_patch,
        raw_exec: request.raw_exec,
        source_snapshot: request.source_snapshot,
        require_paths: request.require_paths,
        validate_require_paths_on_host: true,
    })?;
    let source_snapshot = Some(plan.source_snapshot.clone());
    let path_materialization_plan = request.path_materialization_plan.clone();

    let summary = json!({
        "runner_id": plan.runner.id,
        "cwd": plan.cwd,
        "command": plan.command,
        "capture_patch": request.capture_patch,
        "source_snapshot": source_snapshot,
        "path_materialization_plan": path_materialization_plan,
        "lifecycle": request.lifecycle.clone(),
    });
    let operation = "runner.exec".to_string();
    let run_ref_metadata = exec_request_run_ref_metadata(
        request.lifecycle.as_ref(),
        request.runner_workload.as_ref(),
        request.metadata.as_ref(),
    );
    let runner = job_store
        .run_background_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot.clone(),
            run_ref_metadata,
            path_materialization_plan.clone(),
            move |job| {
                job.progress(json!({
                    "phase": "started",
                    "runner_id": plan.runner.id,
                    "cwd": plan.cwd,
                    "command": plan.command,
                    "capture_patch": request.capture_patch,
                    "job_id": job.job_id(),
                    "source_snapshot": source_snapshot,
                    "path_materialization_plan": path_materialization_plan,
                }))?;
                let baseline = if request.capture_patch {
                    Some(capture_baseline(&plan.cwd)?)
                } else {
                    None
                };
                let progress_job = job.clone();
                let progress_sink = Arc::new(move |data| {
                    let _ = progress_job.progress(data);
                });
                let process_output = execute_runner_process_until_cancelled_with_progress(
                    &plan,
                    || job.is_cancelled(),
                    Some(progress_sink),
                )?;
                let stdout = process_output.stdout.clone();
                let stderr = process_output.stderr.clone();
                let exit_code = process_output.exit_code;
                let metrics = process_output.metrics.clone();
                let capture = process_output.capture.clone();
                if job.is_cancelled() {
                    let _ = job.progress(json!({
                        "phase": "cancelled",
                        "exit_code": exit_code,
                        "metrics": metrics.clone(),
                    }));
                    return Ok(json!({
                        "runner_id": plan.runner.id.clone(),
                        "cwd": plan.cwd.clone(),
                        "command": plan.command.clone(),
                        "exit_code": exit_code,
                        "stdout": stdout,
                        "stderr": stderr,
                        "source_snapshot": source_snapshot,
                        "path_materialization_plan": path_materialization_plan,
                        "metrics": metrics,
                        "capture": capture,
                        "status": JobStatus::Cancelled,
                    }));
                }
                if !stdout.is_empty() {
                    job.stdout(stdout.clone())?;
                }
                if !stderr.is_empty() {
                    job.stderr(stderr.clone())?;
                }
                job.progress(json!({
                    "phase": "finished",
                    "exit_code": exit_code,
                    "metrics": metrics.clone(),
                }))?;
                let patch = if let Some(baseline) = baseline {
                    Some(capture_patch_report(
                        job.job_id(),
                        &plan.runner.id,
                        &plan.cwd,
                        &plan.command,
                        source_snapshot.as_ref(),
                        &baseline,
                        exit_code,
                    )?)
                } else {
                    None
                };
                let result = json!({
                    "runner_id": plan.runner.id,
                    "cwd": plan.cwd,
                    "command": plan.command,
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "source_snapshot": source_snapshot,
                    "path_materialization_plan": path_materialization_plan,
                    "patch": patch,
                    "metrics": metrics,
                    "capture": capture,
                });
                if exit_code != 0 {
                    job.result(result.clone())?;
                    return Err(Error::remote_command_failed(RemoteCommandFailedDetails {
                        command: plan.command.join(" "),
                        exit_code,
                        stdout,
                        stderr,
                        target: TargetDetails {
                            project_id: None,
                            server_id: Some(plan.runner.id.clone()),
                            host: None,
                        },
                    }));
                }

                Ok(result)
            },
        );
    let job = job_store.get(runner.job_id)?;

    Ok(json!({
        "command": "api.runner.exec.enqueue",
        "job": job,
        "poll": {
            "job": format!("/jobs/{}", runner.job_id),
            "events": format!("/jobs/{}/events", runner.job_id),
        },
        "request": summary,
    }))
}

fn exec_request_run_ref_metadata(
    lifecycle: Option<&RunnerJobLifecycleMetadata>,
    runner_workload: Option<&RunnerWorkload>,
    metadata: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let durable_run_id = lifecycle
        .and_then(|lifecycle| non_empty_string(lifecycle.durable_run_id.as_deref()))
        .or_else(|| metadata.and_then(metadata_run_id));
    let agent_task_run_id = runner_workload
        .and_then(|workload| workload.agent_task.as_ref())
        .and_then(|agent_task| non_empty_string(Some(agent_task.run_id.as_str())))
        .or_else(|| {
            metadata
                .and_then(|metadata| metadata.get("agent_task_run_id"))
                .and_then(|run_id| non_empty_string(run_id.as_str()))
        })
        .or_else(|| durable_run_id.clone());

    if durable_run_id.is_none() && agent_task_run_id.is_none() {
        return None;
    }

    Some(json!({
        "durable_run_id": durable_run_id,
        "agent_task_run_id": agent_task_run_id,
    }))
}

fn metadata_run_id(metadata: &serde_json::Value) -> Option<String> {
    ["durable_run_id", "run_id", "record_run_id"]
        .iter()
        .find_map(|key| metadata.get(*key))
        .and_then(|run_id| non_empty_string(run_id.as_str()))
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn daemon_job_store() -> &'static JobStore {
    DAEMON_JOB_STORE.get_or_init(JobStore::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn owner_lock_blocks_a_second_owner_without_lease_or_listener_evidence() {
        with_isolated_home(|_| {
            let first = try_acquire_daemon_owner_lock()
                .expect("acquire owner lock")
                .expect("first owner");
            assert!(try_acquire_daemon_owner_lock()
                .expect("probe owner lock")
                .is_none());
            drop(first);
            assert!(try_acquire_daemon_owner_lock()
                .expect("reacquire owner lock")
                .is_some());
        });
    }

    #[test]
    fn exec_request_run_ref_metadata_prefers_lifecycle_run_id() {
        let lifecycle = RunnerJobLifecycleMetadata {
            source: Some("runner-daemon".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some("agent-task-run-123".to_string()),
            active_child_count: None,
            active_cell_count: None,
        };

        let metadata = exec_request_run_ref_metadata(
            Some(&lifecycle),
            None,
            Some(&json!({ "run_id": "metadata-run" })),
        )
        .expect("run ref metadata");

        assert_eq!(metadata["durable_run_id"], "agent-task-run-123");
        assert_eq!(metadata["agent_task_run_id"], "agent-task-run-123");
    }
}

fn daemon_runtime_snapshot() -> &'static DaemonRuntimeSnapshot {
    DAEMON_RUNTIME_SNAPSHOT.get_or_init(capture_daemon_runtime_snapshot)
}

fn daemon_runtime_paths_body() -> serde_json::Value {
    let snapshot = daemon_runtime_snapshot();
    let current: Vec<_> = snapshot
        .paths
        .iter()
        .map(|path| DaemonRuntimePathSnapshot {
            env: path.env.clone(),
            path: path.path.clone(),
            fingerprint: runtime_path_fingerprint(Path::new(&path.path)),
        })
        .collect();
    let stale: Vec<_> = snapshot
        .paths
        .iter()
        .zip(current.iter())
        .filter(|(loaded, current)| loaded.fingerprint != current.fingerprint)
        .map(|(loaded, current)| {
            json!({
                "env": loaded.env,
                "path": loaded.path,
                "loaded_fingerprint": loaded.fingerprint,
                "current_fingerprint": current.fingerprint,
            })
        })
        .collect();

    json!({
        "loaded_at": snapshot.loaded_at,
        "loaded": snapshot.paths,
        "current": current,
        "stale": stale,
    })
}

fn capture_daemon_runtime_snapshot() -> DaemonRuntimeSnapshot {
    let paths = runtime_path_env_values()
        .into_iter()
        .map(|(env, path)| DaemonRuntimePathSnapshot {
            env,
            fingerprint: runtime_path_fingerprint(Path::new(&path)),
            path,
        })
        .collect();

    DaemonRuntimeSnapshot {
        loaded_at: chrono::Utc::now().to_rfc3339(),
        paths,
    }
}

fn runtime_path_env_values() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, value)| is_runtime_path_env(name) && !value.trim().is_empty())
        .collect()
}

fn is_runtime_path_env(name: &str) -> bool {
    name.starts_with("HOMEBOY_")
        && RUNTIME_PATH_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn runtime_path_fingerprint(path: &Path) -> String {
    let mut state = RuntimePathFingerprintState::default();
    collect_runtime_path_fingerprint(path, &mut state);
    format!(
        "exists={};files={};dirs={};bytes={};mtime_ns={};truncated={}",
        state.exists, state.files, state.dirs, state.bytes, state.latest_mtime_ns, state.truncated
    )
}

#[derive(Default)]
struct RuntimePathFingerprintState {
    exists: bool,
    files: usize,
    dirs: usize,
    bytes: u64,
    latest_mtime_ns: u128,
    truncated: bool,
}

fn collect_runtime_path_fingerprint(path: &Path, state: &mut RuntimePathFingerprintState) {
    if state.files >= RUNTIME_PATH_FILE_LIMIT {
        state.truncated = true;
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    state.exists = true;
    if metadata.is_dir() {
        state.dirs += 1;
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if should_skip_runtime_fingerprint_path(&path) {
                continue;
            }
            collect_runtime_path_fingerprint(&path, state);
            if state.truncated {
                return;
            }
        }
    } else if metadata.is_file() {
        state.files += 1;
        state.bytes = state.bytes.saturating_add(metadata.len());
    }
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
            state.latest_mtime_ns = state.latest_mtime_ns.max(duration.as_nanos());
        }
    }
}

fn should_skip_runtime_fingerprint_path(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "vendor" | "target")
    )
}

fn route_read_only_api(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    job_store: &JobStore,
    analysis_runner: impl AnalysisJobRunner,
) -> HttpResponse {
    let method = match method {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        _ => {
            return HttpResponse {
                status_code: 405,
                body: json!({ "error": "method_not_allowed" }),
                artifact: None,
            };
        }
    };

    if matches!(method, HttpMethod::Get) {
        if let Some(response) = artifact_download::route(path) {
            return response;
        }
    }

    match http_api::handle_with_jobs_and_runner(
        http_api::HttpApiRequest {
            method,
            path: path.to_string(),
            body,
        },
        job_store,
        analysis_runner,
    ) {
        Ok(response) => HttpResponse {
            status_code: response.status,
            body: serde_json::to_value(response)
                .unwrap_or_else(|_| json!({ "error": "internal_json" })),
            artifact: None,
        },
        Err(err) => error_response(404, err),
    }
}

pub(super) fn error_response(status_code: u16, err: Error) -> HttpResponse {
    HttpResponse {
        status_code,
        body: json!({
            "error": err.code.as_str(),
            "message": err.message,
            "details": err.details,
            "hints": err.hints,
        }),
        artifact: None,
    }
}

pub(crate) fn artifact_response_for_path(path: &str) -> Option<HttpResponse> {
    artifact_download::route(path)
}

fn config_paths_body() -> Result<serde_json::Value> {
    Ok(json!({
        "homeboy": paths::homeboy()?.display().to_string(),
        "homeboy_json": paths::homeboy_json()?.display().to_string(),
        "projects": paths::projects()?.display().to_string(),
        "servers": paths::servers()?.display().to_string(),
        "components": paths::components()?.display().to_string(),
        "extensions": paths::extensions()?.display().to_string(),
        "rigs": paths::rigs()?.display().to_string(),
        "stacks": paths::stacks()?.display().to_string(),
        "daemon_state": paths::daemon_state_file()?.display().to_string(),
        "daemon_jobs": paths::daemon_jobs_file()?.display().to_string(),
    }))
}

fn write_state(addr: SocketAddr) -> Result<DaemonState> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let state = DaemonState {
        schema: DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: Uuid::new_v4().to_string(),
        startup_token: std::env::var(DAEMON_STARTUP_TOKEN_ENV).unwrap_or_default(),
        address: addr.to_string(),
        pid: std::process::id(),
        state_path: path.display().to_string(),
        started_at: now.clone(),
        last_seen_at: now,
        build_identity: build_identity::current(),
        binary_sha256: current_binary_sha256()?,
        runtime_paths: capture_daemon_runtime_snapshot(),
    };
    write_lease(&path, &state)?;
    Ok(state)
}

fn heartbeat_lease() -> Result<DaemonState> {
    let path = state_path()?;
    let mut validation = validate_lease_file(&path)?;
    let Some(mut state) = validation
        .state
        .take()
        .filter(|_| validation.fresh && validation.running)
    else {
        return Err(Error::internal_unexpected("daemon lease is not fresh"));
    };
    state.last_seen_at = chrono::Utc::now().to_rfc3339();
    write_lease(&path, &state)?;
    Ok(state)
}

fn write_lease(path: &Path, state: &DaemonState) -> Result<()> {
    if state.schema != DAEMON_LEASE_SCHEMA {
        return Err(Error::internal_unexpected(format!(
            "refusing to write daemon lease with unsupported schema `{}`",
            state.schema
        )));
    }
    let body = serde_json::to_string_pretty(&state).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize daemon state".to_string()))
    })?;
    let parent = path.parent().ok_or_else(|| {
        Error::internal_io(
            "daemon lease path has no parent directory",
            Some(format!("write {}", path.display())),
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state.json"),
        Uuid::new_v4()
    ));
    {
        let mut file = File::create(&temp_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("create {}", temp_path.display())),
            )
        })?;
        file.write_all(body.as_bytes()).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("write {}", temp_path.display())),
            )
        })?;
        file.sync_all().map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("sync {}", temp_path.display())))
        })?;
    }
    fs::rename(&temp_path, path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "rename {} to {}",
                temp_path.display(),
                path.display()
            )),
        )
    })?;
    Ok(())
}

fn acquire_daemon_operation_lock() -> Result<DaemonOperationLock> {
    let state = state_path()?;
    let Some(parent) = state.parent() else {
        return Err(Error::internal_io(
            "daemon state path has no parent directory",
            Some(format!("lock daemon lifecycle for {}", state.display())),
        ));
    };
    fs::create_dir_all(parent).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
    })?;
    let path = parent.join("operation.lock");
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::internal_unexpected(format!(
                "daemon lifecycle operation already in progress; lock file exists at {}",
                path.display()
            ))
            .with_hint(format!(
                "If no homeboy daemon start/stop command is running, remove {} and retry",
                path.display()
            )));
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("create {}", path.display())),
            ));
        }
    };
    let body = format!("pid={}\n", std::process::id());
    file.write_all(body.as_bytes()).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(DaemonOperationLock { path })
}

pub(super) fn try_acquire_daemon_owner_lock() -> Result<Option<DaemonOwnerLock>> {
    let state = state_path()?;
    let parent = state.parent().ok_or_else(|| {
        Error::internal_io(
            "daemon state path has no parent directory",
            Some("lock daemon owner".to_string()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(parent.join("owner.lock"))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open daemon owner lock".to_string()),
            )
        })?;
    #[cfg(unix)]
    unsafe {
        if libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&file),
            libc::LOCK_EX | libc::LOCK_NB,
        ) != 0
        {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock daemon owner".to_string()),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = &file;
    Ok(Some(DaemonOwnerLock { file }))
}

fn acquire_daemon_owner_lock() -> Result<DaemonOwnerLock> {
    try_acquire_daemon_owner_lock()?.ok_or_else(|| {
        Error::internal_unexpected("daemon owner lock is held by a live or starting daemon")
    })
}

impl Drop for DaemonOwnerLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.file), libc::LOCK_UN);
        }
    }
}

/// `ensure-running` is idempotent, so a concurrent caller can wait briefly for
/// the first caller to publish its daemon lease instead of failing spuriously.
/// Destructive lifecycle operations continue using the fail-fast acquisition.
pub(super) fn acquire_daemon_operation_lock_for_ensure(
    wait: Duration,
) -> Result<DaemonOperationLock> {
    const RETRY: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + wait;
    loop {
        match acquire_daemon_operation_lock() {
            Ok(lock) => return Ok(lock),
            Err(err)
                if err
                    .message
                    .contains("daemon lifecycle operation already in progress")
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(RETRY);
            }
            Err(err)
                if err
                    .message
                    .contains("daemon lifecycle operation already in progress") =>
            {
                return Err(Error::internal_unexpected(format!(
                    "timed out after {}s waiting for daemon ensure-running lifecycle lock; another caller may still be starting the daemon",
                    wait.as_secs()
                )));
            }
            Err(err) => return Err(err),
        }
    }
}

impl Drop for DaemonOperationLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn corrupt_daemon_lease_error(path: &Path, reason: Option<String>) -> Error {
    Error::validation_invalid_argument(
        "daemon_lease",
        format!(
            "daemon lease at {} is corrupt{}; refusing to signal any pid from it",
            path.display(),
            reason
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ),
        Some(path.display().to_string()),
        Some(vec![format!(
            "Inspect {} and remove it manually only after verifying no daemon process owns it",
            path.display()
        )]),
    )
}

fn remove_lease_if_identity_matches(path: &Path, expected: &DaemonLeaseIdentity) -> Result<bool> {
    let _ = read_lease_if_identity_matches(path, expected)?;
    fs::remove_file(path).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
    })?;
    Ok(true)
}

fn read_lease_if_identity_matches(
    path: &Path,
    expected: &DaemonLeaseIdentity,
) -> Result<DaemonState> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::internal_unexpected(format!(
                "daemon lease disappeared before lifecycle operation could verify {}",
                path.display()
            )));
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("read {}", path.display())),
            ))
        }
    };
    let state: DaemonState = serde_json::from_str(&content)
        .map_err(|error| corrupt_daemon_lease_error(path, Some(error.to_string())))?;
    if !expected.matches(&state) {
        return Err(Error::internal_unexpected(format!(
            "daemon lease changed while stopping pid {}; refusing to remove {}",
            state.pid,
            path.display()
        )));
    }
    Ok(state)
}

fn current_binary_sha256() -> Result<Option<String>> {
    let exe = std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current executable for daemon lease".to_string()),
        )
    })?;
    sha256_file_optional(&exe)
}

fn sha256_file_optional(path: &Path) -> Result<Option<String>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("open {}", path.display())),
            ))
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn handle_connection<R>(
    mut stream: TcpStream,
    job_store: &JobStore,
    analysis_runner: R,
    loopback_bind: bool,
) -> std::io::Result<()>
where
    R: AnalysisJobRunner,
{
    let request_bytes = read_http_request(&mut stream)?;
    let request = String::from_utf8_lossy(&request_bytes);
    let mut headers_and_body = request.splitn(2, "\r\n\r\n");
    let headers = headers_and_body.next().unwrap_or_default();
    let body = headers_and_body.next().unwrap_or_default();
    let broker_token = crate::core::runner::extract_bearer_token(headers);
    let mut parts = headers
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let parsed_body = if body.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<serde_json::Value>(body.trim()) {
            Ok(value) => Some(value),
            Err(error) => {
                let response = error_response(
                    400,
                    Error::validation_invalid_argument(
                        "body",
                        format!("invalid JSON request body: {error}"),
                        None,
                        None,
                    ),
                );
                return write_http_response(stream, response);
            }
        }
    };
    let broker_auth = remote_runner::BrokerAuthContext {
        token: broker_token,
        loopback_bind,
        trusted_local: false,
    };
    let response = route_with_job_store_and_body_and_runner_and_auth(
        method,
        path,
        parsed_body,
        job_store,
        analysis_runner,
        broker_auth,
    );
    write_http_response(stream, response)
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0; 8 * 1024];
    let headers_end = loop {
        let bytes = stream.read(&mut buffer)?;
        if bytes == 0 {
            return Ok(request);
        }
        request.extend_from_slice(&buffer[..bytes]);
        if let Some(index) = find_header_end(&request) {
            break index;
        }
    };

    let headers = String::from_utf8_lossy(&request[..headers_end]);
    let content_length = http_content_length(&headers).unwrap_or(0);
    let body_start = headers_end + 4;
    let body_bytes = request.len().saturating_sub(body_start);
    let remaining = content_length.saturating_sub(body_bytes);
    if remaining > 0 {
        let mut tail = vec![0; remaining];
        stream.read_exact(&mut tail)?;
        request.extend_from_slice(&tail);
    }

    Ok(request)
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn http_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })
}

fn write_http_response(mut stream: TcpStream, response: HttpResponse) -> std::io::Result<()> {
    if let Some(artifact) = response.artifact {
        return artifact_download::write_response(stream, response.status_code, artifact);
    }

    let success = (200..300).contains(&response.status_code);
    let mut envelope = json!({
        "success": success,
        "data": response.body,
    });
    if !success {
        envelope["error"] = envelope["data"].clone();
    }
    let body = serde_json::to_string_pretty(&envelope)
        .unwrap_or_else(|_| "{\"success\":false}".to_string());
    let status_text = match response.status_code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status_code,
        status_text,
        body.len(),
        body
    )
}

fn terminate_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(format!("stop daemon pid {}", pid)),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(Error::internal_unexpected(
            "daemon stop is not implemented on this platform",
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/core/daemon_test.rs"]
mod daemon_test;
