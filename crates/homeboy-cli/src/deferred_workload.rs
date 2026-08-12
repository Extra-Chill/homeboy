//! Controller-owned durable deferral for portable workloads that have no runner yet.

use fs4::fs_std::FileExt;
use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::content_hash::sha256_hex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "homeboy/deferred-workloads/v1";
pub const CLAIM_LEASE_MS: u64 = 60_000;

/// The environment marker a worker process carries to prove it owns its
/// `--startup-token`.
///
/// This must be present in the worker's **execve** environment. `/proc/<pid>/environ`
/// exposes the environment block the kernel copied at exec time, so a
/// `std::env::set_var` performed by the worker after it starts is invisible to
/// [`pid_has_ownership_token`](homeboy_core::process::pid_has_ownership_token)
/// and the worker can never prove its own liveness. The spawner sets it, and a
/// hand-started worker re-execs itself with it (#12081).
pub const WORKER_OWNER_ENV: &str = "HOMEBOY_DEFERRED_WORKLOAD_OWNER";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkload {
    pub id: String,
    pub fingerprint: String,
    pub command_label: String,
    pub args: Vec<String>,
    pub placement: String,
    pub resource_requirement: String,
    pub portability: String,
    pub reason: String,
    pub ci_alternative: String,
    pub resolved_contract: serde_json::Value,
    pub resolved_resources: serde_json::Value,
    #[serde(default)]
    pub test_requirements: DeferredWorkloadRequirements,
    /// The source worktree this workload was deferred from.
    ///
    /// A deferred record is replayed later by a long-lived singleton worker, so
    /// the replay cannot inherit the deferring caller's working directory —
    /// that is how workers ended up anchored to deleted worktrees, and how a
    /// replay could have synced the wrong source tree. Recording the directory
    /// makes the replay target explicit and lets the worker fail a record whose
    /// worktree is gone instead of running it against whatever it is standing
    /// in (#12081).
    #[serde(default)]
    pub source_directory: Option<String>,
    pub job_overrides: homeboy_core::lab_offload::LabJobOverrides,
    pub state: DeferredWorkloadState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub runner_id: Option<String>,
    pub claim_owner: Option<String>,
    pub claim_expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeferredWorkloadState {
    Deferred,
    Claimed,
    Dispatched,
    Failed,
}

#[derive(Clone, Debug)]
pub struct DeferredWorkloadInput {
    pub command_label: String,
    pub args: Vec<String>,
    pub placement: String,
    pub resource_requirement: String,
    pub portability: String,
    pub reason: String,
    pub ci_alternative: String,
    pub resolved_contract: serde_json::Value,
    pub resolved_resources: serde_json::Value,
    pub test_requirements: DeferredWorkloadRequirements,
    pub source_directory: Option<String>,
    pub job_overrides: homeboy_core::lab_offload::LabJobOverrides,
}

/// Exact runner admission requirements persisted with a deferred workload.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkloadRequirements {
    #[serde(default)]
    pub required_runtimes: BTreeSet<String>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<String>,
}

impl DeferredWorkloadRequirements {
    pub fn is_satisfied_by(
        &self,
        runtime_ids: &BTreeSet<String>,
        capabilities: &BTreeSet<String>,
    ) -> bool {
        self.required_runtimes.is_subset(runtime_ids)
            && self.required_capabilities.is_subset(capabilities)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkloadWorkerStatus {
    pub schema: String,
    pub pid: u32,
    pub owner_token: String,
    pub linux_starttime_ticks: Option<u64>,
    pub state: String,
    pub updated_at_ms: u64,
    pub detail: String,
}

/// The exclusive worker ownership guard. The directory file is deliberately
/// retained for the complete worker lifetime: advisory locks belong to its
/// inode, not to the pathname used to open it.
pub struct DeferredWorkloadWorkerLock {
    _root: File,
}

/// Short-lived admission lock for deciding whether a worker process needs to
/// be spawned. The lifetime worker lock still owns readiness and dispatch.
pub struct DeferredWorkloadWorkerStartLock {
    _file: File,
}

pub fn defer(input: DeferredWorkloadInput) -> Result<DeferredWorkload> {
    if input
        .job_overrides
        .secret_env_names
        .iter()
        .any(|name| input.job_overrides.env.contains_key(name))
    {
        return Err(Error::validation_invalid_argument(
            "job_overrides",
            "deferred workloads cannot persist inline values for runner secret identities",
            None,
            Some(vec![
                "Use a runner-owned secret reference instead of an inline environment value."
                    .to_string(),
            ]),
        ));
    }
    update(|records| {
        let fingerprint = fingerprint(&input)?;
        if let Some(existing) = records.iter().find(|record| {
            record.fingerprint == fingerprint
                && matches!(
                    record.state,
                    DeferredWorkloadState::Deferred | DeferredWorkloadState::Claimed
                )
        }) {
            return Ok(existing.clone());
        }
        let now = now_ms();
        let record = DeferredWorkload {
            id: format!("deferred-{}-{now}", &fingerprint[..16]),
            fingerprint,
            command_label: input.command_label,
            args: input.args,
            placement: input.placement,
            resource_requirement: input.resource_requirement,
            portability: input.portability,
            reason: input.reason,
            ci_alternative: input.ci_alternative,
            resolved_contract: input.resolved_contract,
            resolved_resources: input.resolved_resources,
            test_requirements: input.test_requirements,
            source_directory: input.source_directory,
            job_overrides: input.job_overrides,
            state: DeferredWorkloadState::Deferred,
            created_at_ms: now,
            updated_at_ms: now,
            runner_id: None,
            claim_owner: None,
            claim_expires_at_ms: None,
        };
        records.push(record.clone());
        Ok(record)
    })
}

pub fn claim(
    input: &DeferredWorkloadInput,
    runner_id: &str,
    owner: &str,
) -> Result<Option<DeferredWorkload>> {
    update(|records| {
        let fingerprint = fingerprint(input)?;
        let now = now_ms();
        for record in records
            .iter_mut()
            .filter(|record| record.fingerprint == fingerprint)
        {
            if record.state == DeferredWorkloadState::Claimed
                && record
                    .claim_expires_at_ms
                    .is_some_and(|expiry| expiry <= now)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
            }
        }
        let Some(record) = records.iter_mut().find(|record| {
            record.fingerprint == fingerprint && record.state == DeferredWorkloadState::Deferred
        }) else {
            return Ok(None);
        };
        record.state = DeferredWorkloadState::Claimed;
        record.runner_id = Some(runner_id.to_string());
        record.claim_owner = Some(owner.to_string());
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(Some(record.clone()))
    })
}

/// Atomically claim the next eligible record. Expired claims are returned to
/// the queue before selection so a restarted worker can continue after a crash.
pub fn claim_next(runner_id: &str, owner: &str) -> Result<Option<DeferredWorkload>> {
    claim_next_at(runner_id, owner, now_ms())
}

/// Claim the next record using the supplied clock. The worker uses this seam to
/// make lease recovery deterministic without changing the durable protocol.
pub fn claim_next_at(runner_id: &str, owner: &str, now: u64) -> Result<Option<DeferredWorkload>> {
    claim_next_matching_at(runner_id, owner, now, |_| true)
}

/// Claim the next deferred workload accepted by the selected runner. Records
/// that require a different runtime or capability remain deferred for a later
/// compatible runner.
pub fn claim_next_matching_at(
    runner_id: &str,
    owner: &str,
    now: u64,
    accepts: impl Fn(&DeferredWorkload) -> bool,
) -> Result<Option<DeferredWorkload>> {
    update(|records| {
        for record in records.iter_mut() {
            if record.state == DeferredWorkloadState::Claimed
                && record
                    .claim_expires_at_ms
                    .is_some_and(|expiry| expiry <= now)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
                record.updated_at_ms = now;
            }
        }
        let Some(record) = records
            .iter_mut()
            .find(|record| record.state == DeferredWorkloadState::Deferred && accepts(record))
        else {
            return Ok(None);
        };
        record.state = DeferredWorkloadState::Claimed;
        record.runner_id = Some(runner_id.to_string());
        record.claim_owner = Some(owner.to_string());
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(Some(record.clone()))
    })
}

pub fn heartbeat(id: &str, owner: &str) -> Result<bool> {
    update(|records| {
        let Some(record) = records.iter_mut().find(|record| record.id == id) else {
            return Ok(false);
        };
        if record.state != DeferredWorkloadState::Claimed
            || record.claim_owner.as_deref() != Some(owner)
        {
            return Ok(false);
        }
        let now = now_ms();
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(true)
    })
}

pub fn terminalize(id: &str, succeeded: bool) -> Result<()> {
    update(|records| {
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            record.state = if succeeded {
                DeferredWorkloadState::Dispatched
            } else {
                DeferredWorkloadState::Failed
            };
            record.updated_at_ms = now_ms();
            record.claim_expires_at_ms = None;
            record.claim_owner = None;
        }
        Ok(())
    })
}

/// Return a claimed workload to the queue when runner preflight discovers that
/// the selected runner no longer satisfies its persisted contract.
pub fn defer_claim(id: &str, owner: &str) -> Result<()> {
    update(|records| {
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            if record.state == DeferredWorkloadState::Claimed
                && record.claim_owner.as_deref() == Some(owner)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
                record.updated_at_ms = now_ms();
            }
        }
        Ok(())
    })
}

/// Return every claim held by `owner` to the queue.
///
/// Reconciliation terminates a worker that can no longer prove ownership, so
/// its claims must not sit out the full lease before another worker may take
/// them. Returns the ids that were released.
pub fn release_claims_for_owner(owner: &str) -> Result<Vec<String>> {
    update(|records| {
        let now = now_ms();
        let mut released = Vec::new();
        for record in records.iter_mut() {
            if record.state != DeferredWorkloadState::Claimed
                || record.claim_owner.as_deref() != Some(owner)
            {
                continue;
            }
            record.state = DeferredWorkloadState::Deferred;
            record.runner_id = None;
            record.claim_owner = None;
            record.claim_expires_at_ms = None;
            record.updated_at_ms = now;
            released.push(record.id.clone());
        }
        Ok(released)
    })
}

pub fn records() -> Result<Vec<DeferredWorkload>> {
    read_store(&store_path()?)
}

/// Whether any record still needs a worker.
pub fn has_pending_work() -> Result<bool> {
    Ok(records()?.iter().any(|record| {
        matches!(
            record.state,
            DeferredWorkloadState::Deferred | DeferredWorkloadState::Claimed
        )
    }))
}

/// The directory whose inode carries the singleton worker lock.
///
/// It doubles as the worker's working directory: a controller-owned singleton
/// that outlives the command which started it must not hold a caller's
/// ephemeral worktree open, because worktree cleanup then leaves the process
/// anchored to a deleted directory (#12081).
pub fn worker_root() -> Result<PathBuf> {
    let root = homeboy_core::paths::homeboy()?;
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create deferred workload root {}", root.display())),
        )
    })?;
    fs::canonicalize(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "canonicalize deferred workload root {}",
                root.display()
            )),
        )
    })
}

pub fn try_acquire_worker_lock() -> Result<Option<DeferredWorkloadWorkerLock>> {
    let root = worker_root()?;
    let file = File::open(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("open deferred workload root {}", root.display())),
        )
    })?;
    match file.try_lock_exclusive() {
        Ok(true) => Ok(Some(DeferredWorkloadWorkerLock { _root: file })),
        Ok(false) => Ok(None),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!(
                "acquire deferred workload worker lock {}",
                root.display()
            )),
        )),
    }
}

pub fn acquire_worker_start_lock() -> Result<DeferredWorkloadWorkerStartLock> {
    let root = homeboy_core::paths::homeboy()?;
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create deferred workload root {}", root.display())),
        )
    })?;
    let path = root.join("deferred-workload-worker-start.lock");
    let file = OpenOptions::new()
        .create(true)
        // This is an advisory lock file; retain its contents for concurrent holders.
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("open {}", path.display())))
        })?;
    file.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "acquire deferred workload start lock {}",
                path.display()
            )),
        )
    })?;
    Ok(DeferredWorkloadWorkerStartLock { _file: file })
}

pub fn worker_status() -> Result<Option<DeferredWorkloadWorkerStatus>> {
    let path = store_path()?.with_extension("worker-status.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

pub fn worker_is_live(status: &DeferredWorkloadWorkerStatus) -> bool {
    if matches!(status.state.as_str(), "idle" | "stopped") {
        return false;
    }
    if status.owner_token.is_empty() {
        return false;
    }
    let Ok(lock) = try_acquire_worker_lock() else {
        return false;
    };
    // A status file is advisory. The singleton lock is the authority.
    if lock.is_some() {
        return false;
    }
    worker_identity_is_live(
        status,
        homeboy_core::process::process_identity_state,
        |pid, token| {
            homeboy_core::process::pid_has_ownership_token(pid, WORKER_OWNER_ENV, token)
                .unwrap_or(false)
        },
    )
}

fn worker_identity_is_live(
    status: &DeferredWorkloadWorkerStatus,
    inspect_process: impl FnOnce(u32, Option<u64>) -> homeboy_core::process::ProcessIdentityState,
    owns_token: impl FnOnce(u32, &str) -> bool,
) -> bool {
    if cfg!(target_os = "linux") && status.linux_starttime_ticks.is_none() {
        return false;
    }
    inspect_process(status.pid, status.linux_starttime_ticks)
        == homeboy_core::process::ProcessIdentityState::Live
        && owns_token(status.pid, &status.owner_token)
}

/// A live process presenting as a deferred-workload worker.
///
/// Presenting is not owning. The command line only selects candidates; whether
/// a candidate may keep running is decided by [`classify_worker_process`] from
/// the startup token it can prove and the durable store, never from its name.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeferredWorkloadWorkerProcess {
    pub pid: u32,
    /// The `--startup-token` value read from the process command line.
    pub startup_token: Option<String>,
    /// Whether the process environment proves it owns [`startup_token`](Self::startup_token).
    pub owns_startup_token: bool,
    pub working_directory: Option<String>,
    /// Whether the working directory has been unlinked since the process started.
    pub working_directory_deleted: bool,
}

/// What reconciliation may do with a candidate worker process.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum DeferredWorkloadWorkerDisposition {
    /// The live singleton owner with durable work in front of it.
    Retained,
    /// No live durable ownership backs this process.
    Orphaned { reason: String },
}

impl DeferredWorkloadWorkerDisposition {
    fn orphaned(reason: &str) -> Self {
        Self::Orphaned {
            reason: reason.to_string(),
        }
    }

    pub fn is_orphaned(&self) -> bool {
        matches!(self, Self::Orphaned { .. })
    }
}

/// Decide whether a candidate worker process is the live singleton owner.
///
/// Every rejection is grounded in durable state: the record store says whether
/// any work remains, and the worker status plus the process's own execve
/// environment say which process owns it. A process that merely looks like a
/// worker is never retained on that basis, and never terminated on it either.
pub fn classify_worker_process(
    process: &DeferredWorkloadWorkerProcess,
    owner: Option<&DeferredWorkloadWorkerStatus>,
    owner_is_live: bool,
    pending_work: bool,
) -> DeferredWorkloadWorkerDisposition {
    if !pending_work {
        return DeferredWorkloadWorkerDisposition::orphaned(
            "no deferred workload remains for a worker to run",
        );
    }
    let Some(owner) = owner.filter(|_| owner_is_live) else {
        return DeferredWorkloadWorkerDisposition::orphaned(
            "no live worker owns the deferred workload singleton",
        );
    };
    if process.pid != owner.pid {
        return DeferredWorkloadWorkerDisposition::orphaned(
            "process is not the durable singleton owner pid",
        );
    }
    if !process.owns_startup_token {
        return DeferredWorkloadWorkerDisposition::orphaned(
            "process environment does not prove ownership of its startup token",
        );
    }
    if process.startup_token.as_deref() != Some(owner.owner_token.as_str()) {
        return DeferredWorkloadWorkerDisposition::orphaned(
            "startup token does not match the durable owner token",
        );
    }
    DeferredWorkloadWorkerDisposition::Retained
}

/// Every live process whose command line presents as a deferred-workload worker.
pub fn worker_processes() -> Result<Vec<DeferredWorkloadWorkerProcess>> {
    let mut processes = worker_process_candidates()?
        .into_iter()
        .map(|(pid, argv)| {
            let startup_token = worker_startup_token(&argv);
            let owns_startup_token = startup_token.as_deref().is_some_and(|token| {
                homeboy_core::process::pid_has_ownership_token(pid, WORKER_OWNER_ENV, token)
                    .unwrap_or(false)
            });
            let (working_directory, working_directory_deleted) = process_working_directory(pid);
            DeferredWorkloadWorkerProcess {
                pid,
                startup_token,
                owns_startup_token,
                working_directory,
                working_directory_deleted,
            }
        })
        .collect::<Vec<_>>();
    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

/// The `--startup-token` a candidate was started with, in either spelling clap accepts.
///
/// This reads another process's command line, not this invocation's argv, so
/// the bare-separator rule that governs Homeboy-owned argument scans does not
/// apply: there is no forwarded tail here to mistake for Homeboy's own.
fn worker_startup_token(command_line: &[String]) -> Option<String> {
    command_line
        .iter()
        .enumerate()
        .find_map(|(index, argument)| {
            if argument == "--startup-token" {
                command_line.get(index + 1).cloned()
            } else {
                argument
                    .strip_prefix("--startup-token=")
                    .map(ToString::to_string)
            }
        })
}

/// Whether a process command line is a `deferred-workload worker` invocation.
fn presents_as_worker(command_line: &[String]) -> bool {
    command_line
        .windows(2)
        .any(|pair| pair == ["deferred-workload", "worker"])
}

#[cfg(target_os = "linux")]
fn worker_process_candidates() -> Result<Vec<(u32, Vec<String>)>> {
    let entries = fs::read_dir("/proc").map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("enumerate deferred workload worker processes".to_string()),
        )
    })?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process can exit mid-scan; an unreadable entry is simply not a
        // candidate rather than a scan failure.
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv = cmdline
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8_lossy(argument).into_owned())
            .collect::<Vec<_>>();
        if presents_as_worker(&argv) {
            candidates.push((pid, argv));
        }
    }
    Ok(candidates)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn worker_process_candidates() -> Result<Vec<(u32, Vec<String>)>> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("enumerate deferred workload worker processes".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(
            "enumerate deferred workload worker processes: ps failed",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let argv = fields.map(ToString::to_string).collect::<Vec<_>>();
            presents_as_worker(&argv).then_some((pid, argv))
        })
        .collect())
}

#[cfg(not(unix))]
fn worker_process_candidates() -> Result<Vec<(u32, Vec<String>)>> {
    Err(Error::validation_invalid_argument(
        "platform",
        "deferred workload worker reconciliation requires a Unix process table",
        None,
        None,
    ))
}

/// The working directory a process holds open, and whether it has been unlinked.
///
/// Linux renders an unlinked directory as `<path> (deleted)` in `/proc/<pid>/cwd`,
/// which is the exact evidence that a finalized worktree is still pinned.
#[cfg(target_os = "linux")]
fn process_working_directory(pid: u32) -> (Option<String>, bool) {
    let Ok(target) = fs::read_link(format!("/proc/{pid}/cwd")) else {
        return (None, false);
    };
    let target = target.to_string_lossy().into_owned();
    match target.strip_suffix(" (deleted)") {
        Some(path) => (Some(path.to_string()), true),
        None => (Some(target), false),
    }
}

#[cfg(not(target_os = "linux"))]
fn process_working_directory(_pid: u32) -> (Option<String>, bool) {
    (None, false)
}

pub fn write_worker_status(
    owner_token: &str,
    state: &str,
    detail: impl Into<String>,
) -> Result<()> {
    let path = store_path()?.with_extension("worker-status.json");
    let value = DeferredWorkloadWorkerStatus {
        schema: "homeboy/deferred-workload-worker-status/v1".to_string(),
        pid: std::process::id(),
        owner_token: owner_token.to_string(),
        linux_starttime_ticks: homeboy_core::process::linux_process_starttime_ticks(
            std::process::id(),
        )
        .ok()
        .flatten(),
        state: state.to_string(),
        updated_at_ms: now_ms(),
        detail: detail.into(),
    };
    let bytes = serde_json::to_vec(&value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize deferred workload worker status".to_string()),
        )
    })?;
    write_store(&path, &bytes)
}

pub fn append_worker_log(message: impl AsRef<str>) -> Result<()> {
    let path = store_path()?.with_extension("worker.log");
    let line = format!("{} {}\n", now_ms(), message.as_ref());
    use std::io::Write;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("append {}", path.display())),
            )
        })
}

fn fingerprint(input: &DeferredWorkloadInput) -> Result<String> {
    let value = serde_json::to_vec(&serde_json::json!({
        "command_label": input.command_label,
        "args": input.args,
        "placement": input.placement,
        "resource_requirement": input.resource_requirement,
        // Two identical commands deferred from two worktrees are two workloads.
        // Leaving the source directory out of the identity collapsed them into
        // one record, so the second caller's tree was silently never replayed.
        "source_directory": input.source_directory,
        "job_overrides": input.job_overrides,
    }))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize deferred workload".to_string()),
        )
    })?;
    Ok(sha256_hex(&value))
}

fn store_path() -> Result<PathBuf> {
    Ok(homeboy_core::paths::homeboy()?.join("deferred-workloads.json"))
}

fn update<T>(mutate: impl FnOnce(&mut Vec<DeferredWorkload>) -> Result<T>) -> Result<T> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create deferred workload store".to_string()),
            )
        })?;
    }
    let lock = OpenOptions::new()
        .create(true)
        // This is an advisory lock file, not the workload store being replaced below.
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open deferred workload lock".to_string()),
            )
        })?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock deferred workload store".to_string()),
        )
    })?;
    let mut records = read_store(&path)?;
    let output = mutate(&mut records)?;
    let bytes = serde_json::to_vec(&serde_json::json!({ "schema": SCHEMA, "records": records }))
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize deferred workload store".to_string()),
            )
        })?;
    write_store(&path, &bytes)?;
    let _ = lock.unlock();
    Ok(output)
}

fn read_store(path: &Path) -> Result<Vec<DeferredWorkload>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCHEMA) {
        return Err(Error::validation_invalid_argument(
            "deferred_workload_store",
            "unrecognized deferred workload store schema",
            Some(path.display().to_string()),
            None,
        ));
    }
    serde_json::from_value(value.get("records").cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "deferred_workload_store",
            "missing records",
            Some(path.display().to_string()),
            None,
        )
    })?)
    .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

fn write_store(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("deferred-workloads.json"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temporary, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", temporary.display())),
            )
        })?;
    fs::rename(&temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", temporary.display())),
        )
    })?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    fn input() -> DeferredWorkloadInput {
        DeferredWorkloadInput {
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "no ready runner".to_string(),
            ci_alternative: "run in CI".to_string(),
            resolved_contract: serde_json::json!({ "portability": "portable_lab_route" }),
            resolved_resources: serde_json::json!({ "severity": "warm" }),
            test_requirements: DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: ["review test".to_string()].into(),
            },
            source_directory: None,
            job_overrides: homeboy_core::lab_offload::LabJobOverrides::default(),
        }
    }

    fn owner_status(pid: u32, owner_token: &str) -> DeferredWorkloadWorkerStatus {
        DeferredWorkloadWorkerStatus {
            schema: "homeboy/deferred-workload-worker-status/v1".to_string(),
            pid,
            owner_token: owner_token.to_string(),
            linux_starttime_ticks: Some(1),
            state: "waiting_for_runner".to_string(),
            updated_at_ms: 0,
            detail: String::new(),
        }
    }

    fn worker_process(pid: u32, owner_token: &str) -> DeferredWorkloadWorkerProcess {
        DeferredWorkloadWorkerProcess {
            pid,
            startup_token: Some(owner_token.to_string()),
            owns_startup_token: true,
            working_directory: None,
            working_directory_deleted: false,
        }
    }

    #[test]
    fn deferred_workload_is_idempotent_and_survives_restart_before_claim() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let first = defer(input()).expect("defer workload");
            let replay = defer(input()).expect("replay deferred workload");
            assert_eq!(first.id, replay.id);
            assert_eq!(replay.state, DeferredWorkloadState::Deferred);

            let claimed = claim(&input(), "warm-lab", "first-owner")
                .expect("claim workload")
                .expect("pending workload");
            assert_eq!(claimed.id, first.id);
            assert_eq!(claimed.runner_id.as_deref(), Some("warm-lab"));
            assert!(claim(&input(), "other-lab", "second-owner")
                .expect("idempotent claim")
                .is_none());
        });
    }

    #[test]
    fn reading_an_absent_store_does_not_create_runtime_state() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path = store_path().expect("store path");

            assert!(records().expect("read absent store").is_empty());
            assert!(!path.exists(), "read created deferred workload store");
            assert!(
                !path.with_extension("lock").exists(),
                "read created deferred workload lock"
            );
        });
    }

    #[test]
    fn terminalized_workload_does_not_reappear_as_a_ghost() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let deferred = defer(input()).expect("defer workload");
            let claimed = claim(&input(), "warm-lab", "owner")
                .expect("claim workload")
                .expect("pending workload");
            terminalize(&claimed.id, true).expect("terminalize workload");
            assert!(claim(&input(), "warm-lab", "other-owner")
                .expect("claim after terminal state")
                .is_none());

            let next = defer(input()).expect("new explicit workload after terminal state");
            assert_ne!(
                next.id, deferred.id,
                "terminal work must not be revived by a replay"
            );
        });
    }

    #[test]
    fn expired_claim_is_reclaimed_after_a_post_claim_crash() {
        homeboy_core::test_support::with_isolated_home(|_| {
            defer(input()).expect("defer workload");
            let claimed = claim(&input(), "first-lab", "crashed-owner")
                .expect("claim workload")
                .expect("pending workload");
            update(|records| {
                let record = records
                    .iter_mut()
                    .find(|record| record.id == claimed.id)
                    .expect("claimed record");
                record.claim_expires_at_ms = Some(0);
                Ok(())
            })
            .expect("expire crashed claim");

            let recovered = claim(&input(), "warm-lab", "recovery-owner")
                .expect("reclaim workload")
                .expect("expired claim is reclaimable");
            assert_eq!(recovered.runner_id.as_deref(), Some("warm-lab"));
            assert_eq!(recovered.claim_owner.as_deref(), Some("recovery-owner"));
        });
    }

    #[test]
    fn next_claim_heartbeats_and_publishes_durable_worker_status() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let deferred = defer(input()).expect("defer workload");
            let claimed = claim_next("ready-runner", "worker-a")
                .expect("claim next")
                .expect("deferred workload");
            assert_eq!(claimed.id, deferred.id);
            assert_eq!(claimed.runner_id.as_deref(), Some("ready-runner"));
            assert!(heartbeat(&claimed.id, "worker-a").expect("heartbeat"));
            assert!(!heartbeat(&claimed.id, "worker-b").expect("wrong worker heartbeat"));

            write_worker_status("test-owner", "dispatching", "replaying deferred workload")
                .expect("write worker status");
            let status = worker_status()
                .expect("read worker status")
                .expect("status exists");
            assert_eq!(status.state, "dispatching");
            assert_eq!(status.detail, "replaying deferred workload");
            assert_eq!(status.owner_token, "test-owner");
        });
    }

    #[test]
    fn matching_claim_skips_a_live_claimed_head_for_a_later_deferred_record() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let first = defer(input()).expect("first workload");
            let mut later_input = input();
            later_input.args.push("later".to_string());
            let later = defer(later_input).expect("later workload");
            claim_next_at("other-runner", "other-worker", 1)
                .expect("claim first")
                .expect("first workload is claimed");

            let claimed =
                claim_next_matching_at("ready-runner", "worker", 2, |record| record.id == later.id)
                    .expect("claim matching workload")
                    .expect("later deferred workload is claimable");

            assert_eq!(claimed.id, later.id);
            assert_ne!(claimed.id, first.id);
        });
    }

    #[test]
    fn reused_pid_with_a_different_start_identity_is_not_live() {
        let status = DeferredWorkloadWorkerStatus {
            schema: "homeboy/deferred-workload-worker-status/v1".to_string(),
            pid: 42,
            owner_token: "worker-token".to_string(),
            linux_starttime_ticks: Some(100),
            state: "dispatching".to_string(),
            updated_at_ms: 0,
            detail: String::new(),
        };

        assert!(!worker_identity_is_live(
            &status,
            |_, _| homeboy_core::process::ProcessIdentityState::IdentityMismatch,
            |_, _| true,
        ));
    }

    #[test]
    fn worker_lock_survives_replacement_of_the_legacy_adjacent_lock_file() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let owner = try_acquire_worker_lock()
                .expect("acquire worker lock")
                .expect("first worker owns lock");
            let legacy_lock = store_path()
                .expect("store path")
                .with_extension("worker.lock");
            std::fs::write(&legacy_lock, b"old lock inode").expect("write old lock file");
            let replacement = legacy_lock.with_extension("replacement");
            std::fs::write(&replacement, b"replacement lock inode")
                .expect("write replacement lock file");
            std::fs::rename(&replacement, &legacy_lock).expect("replace legacy lock file");

            assert!(
                try_acquire_worker_lock()
                    .expect("check competing worker")
                    .is_none(),
                "replacing the old lock pathname must not create a second owner"
            );
            drop(owner);
        });
    }

    #[test]
    fn worker_lock_contenders_reach_readiness_once_across_processes() {
        let child_id = std::env::var("HOMEBOY_WORKER_LOCK_TEST_CHILD").ok();
        if let Some(child_id) = child_id {
            let root =
                PathBuf::from(std::env::var("HOMEBOY_WORKER_LOCK_TEST_ROOT").expect("test root"));
            std::fs::write(root.join(format!("ready-{child_id}")), b"ready")
                .expect("announce child readiness");
            while !root.join("start").exists() {
                thread::sleep(Duration::from_millis(5));
            }
            if let Some(_owner) = try_acquire_worker_lock().expect("acquire worker lock") {
                std::fs::write(root.join(format!("readiness-{child_id}")), b"polled")
                    .expect("record readiness polling");
                thread::sleep(Duration::from_millis(250));
            } else {
                std::fs::write(root.join(format!("exited-{child_id}")), b"lost")
                    .expect("record losing contender");
            }
            return;
        }

        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let marker_root = temp.path().join("markers");
        std::fs::create_dir_all(&marker_root).expect("create marker root");
        let test_name = "deferred_workload::tests::worker_lock_contenders_reach_readiness_once_across_processes";
        let mut children = Vec::new();
        for child_id in 0..8 {
            children.push(
                Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", test_name, "--nocapture"])
                    .env("HOME", &home)
                    .env("HOMEBOY_WORKER_LOCK_TEST_ROOT", &marker_root)
                    .env("HOMEBOY_WORKER_LOCK_TEST_CHILD", child_id.to_string())
                    .spawn()
                    .expect("spawn worker contender"),
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_dir(&marker_root)
            .expect("read marker root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-"))
            .count()
            < children.len()
        {
            assert!(Instant::now() < deadline, "worker contenders did not start");
            thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(marker_root.join("start"), b"start").expect("release contenders");
        for mut child in children {
            assert!(child.wait().expect("wait for contender").success());
        }
        let entries = std::fs::read_dir(&marker_root)
            .expect("read marker root")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            entries
                .iter()
                .filter(|name| name.starts_with("readiness-"))
                .count(),
            1,
            "only the owner may reach readiness polling"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|name| name.starts_with("exited-"))
                .count(),
            7,
            "every losing contender exits before readiness polling"
        );
    }

    #[test]
    fn corrupt_store_fails_closed_without_resetting_records() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path = store_path().expect("store path");
            std::fs::create_dir_all(path.parent().expect("store parent")).expect("create parent");
            std::fs::write(&path, b"not-json").expect("write corrupt store");
            assert!(defer(input()).is_err());
            assert_eq!(
                std::fs::read(&path).expect("corrupt bytes remain"),
                b"not-json"
            );
        });
    }

    #[test]
    fn refuses_inline_values_for_runner_secret_identities() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut input = input();
            input.job_overrides.env.insert(
                "DB_SERVICE_PASSWORD".to_string(),
                "fixture-password".to_string(),
            );
            input.job_overrides.secret_env_names = vec!["DB_SERVICE_PASSWORD".to_string()];

            assert!(defer(input).is_err());
            assert!(records().expect("records").is_empty());
        });
    }

    /// The same command deferred from two worktrees is two workloads. Collapsing
    /// them dropped the second caller's tree on the floor, because a record
    /// carries the worktree it must be replayed against.
    #[test]
    fn two_worktrees_deferring_the_same_command_are_two_records() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut first = input();
            first.source_directory = Some("/workspace/repo@one".to_string());
            let mut second = input();
            second.source_directory = Some("/workspace/repo@two".to_string());

            let first = defer(first).expect("defer first worktree");
            let second = defer(second).expect("defer second worktree");

            assert_ne!(first.id, second.id);
            assert_eq!(
                first.source_directory.as_deref(),
                Some("/workspace/repo@one")
            );
            assert_eq!(records().expect("records").len(), 2);
        });
    }

    /// A record deferred before `source_directory` existed must still load.
    #[test]
    fn a_record_without_a_recorded_source_directory_still_loads() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let record = defer(input()).expect("defer workload");
            let mut value = serde_json::to_value(&record).expect("record serializes");
            value
                .as_object_mut()
                .expect("record object")
                .remove("source_directory");
            let path = store_path().expect("store path");
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({ "schema": SCHEMA, "records": [value] }))
                    .expect("legacy store serializes"),
            )
            .expect("write legacy store");

            let loaded = records().expect("read legacy store");
            assert_eq!(loaded.len(), 1);
            assert!(loaded[0].source_directory.is_none());
        });
    }

    #[test]
    fn terminating_a_worker_returns_its_claims_to_the_queue() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let deferred = defer(input()).expect("defer workload");
            claim_next_at("runner", "doomed-worker", 1).expect("claim workload");

            let released = release_claims_for_owner("doomed-worker").expect("release claims");

            assert_eq!(released, vec![deferred.id]);
            assert_eq!(
                records().expect("records")[0].state,
                DeferredWorkloadState::Deferred
            );
            assert!(records().expect("records")[0].claim_owner.is_none());
        });
    }

    #[test]
    fn only_the_proven_singleton_owner_survives_reconciliation() {
        let owner = owner_status(41, "owner-token");

        assert_eq!(
            classify_worker_process(&worker_process(41, "owner-token"), Some(&owner), true, true),
            DeferredWorkloadWorkerDisposition::Retained
        );

        // A different process running the same command is not the owner.
        assert!(classify_worker_process(
            &worker_process(42, "owner-token"),
            Some(&owner),
            true,
            true
        )
        .is_orphaned());
        // The owner pid presenting a token it cannot prove is not the owner
        // either: `/proc/<pid>/environ` is the proof, argv is only the label.
        let unproven = DeferredWorkloadWorkerProcess {
            owns_startup_token: false,
            ..worker_process(41, "owner-token")
        };
        assert!(classify_worker_process(&unproven, Some(&owner), true, true).is_orphaned());
        // A stale token from a previous singleton generation is not the owner.
        assert!(classify_worker_process(
            &worker_process(41, "stale-token"),
            Some(&owner),
            true,
            true
        )
        .is_orphaned());
    }

    #[test]
    fn a_worker_with_no_durable_work_or_no_live_owner_is_orphaned() {
        let owner = owner_status(41, "owner-token");

        assert!(classify_worker_process(
            &worker_process(41, "owner-token"),
            Some(&owner),
            true,
            false
        )
        .is_orphaned());
        assert!(classify_worker_process(
            &worker_process(41, "owner-token"),
            Some(&owner),
            false,
            true
        )
        .is_orphaned());
        assert!(
            classify_worker_process(&worker_process(41, "owner-token"), None, true, true)
                .is_orphaned()
        );
    }

    #[test]
    fn worker_candidates_are_selected_by_command_and_keyed_by_startup_token() {
        assert!(presents_as_worker(&[
            "/usr/local/bin/homeboy".to_string(),
            "deferred-workload".to_string(),
            "worker".to_string(),
        ]));
        assert!(!presents_as_worker(&[
            "/usr/local/bin/homeboy".to_string(),
            "deferred-workload".to_string(),
            "status".to_string(),
        ]));
        assert_eq!(
            worker_startup_token(&[
                "homeboy".to_string(),
                "deferred-workload".to_string(),
                "worker".to_string(),
                "--startup-token".to_string(),
                "token-a".to_string(),
            ]),
            Some("token-a".to_string())
        );
        assert_eq!(
            worker_startup_token(&["--startup-token=token-b".to_string()]),
            Some("token-b".to_string())
        );
        assert_eq!(worker_startup_token(&["worker".to_string()]), None);
    }

    /// The scan must survive a live process table without classifying anything
    /// on command name alone.
    #[test]
    #[cfg(unix)]
    fn scanning_the_process_table_never_reports_this_test_as_a_worker() {
        let processes = worker_processes().expect("scan process table");
        assert!(processes
            .iter()
            .all(|process| process.pid != std::process::id()));
    }
}
