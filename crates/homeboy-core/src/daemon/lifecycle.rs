//! Daemon lifecycle observation (#15557, slice C1).
//!
//! [`observe`] reads every daemon generation recorded in one router directory
//! and returns a [`DaemonView`]: one value that states, for each generation,
//! whether it is admitting, draining, stopped, or dead (and the proof), who
//! supervises it, whether its binary is current, and who holds custody of each
//! active job.
//!
//! The view is built only from the readers the recovery commands already use
//! (`read_status_for_state_path`, the generation registry, the job store's
//! recovery evidence, the process table). It does not parse any file format
//! itself, and it never mutates state. It is total: missing, unreadable, or
//! corrupt inputs become explicit variants instead of errors or panics.
//!
//! Nothing calls `observe` yet. Slice C2 adds a planner over this view; later
//! slices move `daemon recover` and its sibling entry points onto that planner
//! so every command reads liveness from one place instead of re-deriving it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use homeboy_engine_primitives::rolling_generation::RollingDrainState;
use serde::Serialize;
use uuid::Uuid;

use super::{
    generation_store, read_status_for_state_path, DaemonProcessOwnership, DaemonStaleReasonCode,
    DaemonStatus,
};
use crate::api_jobs::{
    DaemonActiveJobRecoveryDisposition, DaemonActiveJobRecoveryEvidence, JobStatus,
};

/// Everything known about the daemons of one router directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonView {
    pub router_dir: PathBuf,
    pub registry: RegistryObservation,
    /// The generation the registry routes new work to, when a registry exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_owner: Option<String>,
    /// Registered generations in registry order, then any lease found in a
    /// generation directory that the registry does not list.
    pub generations: Vec<GenerationView>,
    /// Live daemon processes that may own one of these stores but match no
    /// generation lease. Zombies are excluded: they cannot serve.
    pub unleased_candidates: Vec<UnleasedCandidate>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RegistryObservation {
    Present,
    /// No `generations.json`: a daemon that never handed off. The router
    /// directory's own lease, if any, is reported as an unregistered generation.
    Absent,
    Unreadable {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GenerationView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    pub state_dir: PathBuf,
    /// Whether the generation registry lists this lease.
    pub registered: bool,
    pub lifecycle: GenerationLifecycle,
    pub process: ProcessObservation,
    pub supervision: Supervision,
    pub binary: BinaryFreshness,
    pub jobs: Vec<JobCustodyView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum GenerationLifecycle {
    Admitting,
    Draining,
    /// The generation directory holds no lease.
    Stopped,
    Dead {
        proof: DeathProof,
    },
    /// The lease or job store could not be read or decoded.
    Unreadable {
        reason: String,
    },
}

/// Why a generation is known not to be serving.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeathProof {
    /// The kernel reports the lease PID gone (or a zombie).
    PidDead,
    /// The generation's directory now holds a different lease. The owner lock
    /// of a directory is exclusive, so the registered lease was restarted in
    /// place and cannot still be serving (#15558).
    SameDirReplaced { current_lease_id: String },
    /// The supervisor recorded termination evidence for this exact lease.
    TerminationEvidence,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcessObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub liveness: Liveness,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Live,
    /// Exited but not reaped. Never live (#15558).
    Zombie,
    Dead,
    /// No PID is recorded for this generation.
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Supervision {
    /// Launched by Homeboy's supervisor with a startup credential; a
    /// lease-bound stop may signal it.
    HomeboySupervised,
    /// A lease without a startup credential (`daemon serve` under systemd).
    /// Stop it through its lifecycle endpoint, never by PID (#15437).
    External,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum BinaryFreshness {
    Current,
    /// The live process runs an executable that has since been replaced or
    /// deleted on disk (#15427).
    Replaced,
    /// The live lease was written by a different build or runtime.
    Stale {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason_code: Option<DaemonStaleReasonCode>,
    },
    /// No live process to compare.
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JobCustodyView {
    pub job_id: Uuid,
    pub operation: String,
    /// The job store's own classification, kept verbatim for traceability.
    pub disposition: DaemonActiveJobRecoveryDisposition,
    pub custody: JobCustody,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum JobCustody {
    /// A live child process or a live owning daemon is executing it.
    Running,
    /// Checkpointed in-daemon controller work; a replacement daemon resumes
    /// it from the checkpoint, so it never needs attestation (#15426).
    Resumable,
    /// A recorded result or terminal linked run already settles it.
    TerminalEvidence {
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<JobStatus>,
    },
    /// Nothing executes it any more. `proof: None` means nothing on disk
    /// proves the workload is gone, so recovery needs an operator attestation.
    Orphaned { proof: Option<DeathProof> },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UnleasedCandidate {
    pub pid: u32,
    pub ownership: DaemonProcessOwnership,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_endpoint: Option<String>,
}

/// Observe every generation of the daemon router rooted at `router_dir`.
///
/// Never panics and never fails: each unreadable input is reported in the view.
pub fn observe(router_dir: &Path) -> DaemonView {
    let (registry, admission_owner, registered) = match generation_store::generations_at(router_dir)
    {
        Ok(Some(generations)) => (
            RegistryObservation::Present,
            Some(generations.admission_owner),
            generations
                .generations
                .into_iter()
                .map(|(lease_id, generation)| Registered {
                    lease_id,
                    state_dir: PathBuf::from(generation.endpoint.state_dir),
                    drain_state: generation.drain_state,
                })
                .collect::<Vec<_>>(),
        ),
        Ok(None) => (RegistryObservation::Absent, None, Vec::new()),
        Err(error) => (
            RegistryObservation::Unreadable {
                reason: error.to_string(),
            },
            None,
            Vec::new(),
        ),
    };

    // Every directory that can hold a lease: each registered generation's, and
    // the router directory itself (the first generation's home, or the only
    // one when no registry exists). Each is read once.
    let mut directories: Vec<PathBuf> = Vec::new();
    for generation in &registered {
        push_unique(&mut directories, &generation.state_dir);
    }
    push_unique(&mut directories, router_dir);
    let statuses: Vec<(PathBuf, StatusRead)> = directories
        .iter()
        .map(|dir| {
            (
                dir.clone(),
                read_status_for_state_path(dir.join("state.json")).map_err(|e| e.to_string()),
            )
        })
        .collect();
    // Every registered directory was read above. The fallback only covers a
    // directory whose canonical path changed between the two comparisons.
    let unobserved: StatusRead = Err("generation directory changed while it was observed".into());
    let status_of = |dir: &Path| -> &StatusRead {
        statuses
            .iter()
            .find(|(known, _)| super::same_directory(known, dir))
            .map_or(&unobserved, |(_, status)| status)
    };

    let registered_leases: BTreeSet<&str> =
        registered.iter().map(|g| g.lease_id.as_str()).collect();
    let mut generations = Vec::new();
    for generation in &registered {
        generations.push(generation_view(
            Some(&generation.lease_id),
            &generation.state_dir,
            true,
            Some(generation.drain_state),
            status_of(&generation.state_dir),
        ));
    }
    // A directory can also hold something the registry does not list: the
    // lease of an in-place restart that has not claimed admission yet, the
    // only lease of a daemon that never handed off, or an unreadable lease in
    // a directory no registered generation covers.
    for (dir, status) in &statuses {
        let covered = registered
            .iter()
            .any(|g| super::same_directory(&g.state_dir, dir));
        let unlisted_view = match status {
            Ok(status) => match status.state.as_ref() {
                Some(state) => !registered_leases.contains(state.lease_id.as_str()),
                None => {
                    !covered
                        && status.freshness.stale_reason_code
                            == Some(DaemonStaleReasonCode::LeaseCorrupt)
                }
            },
            Err(_) => !covered,
        };
        if unlisted_view {
            let occupant = status
                .as_ref()
                .ok()
                .and_then(|status| status.state.as_ref())
                .map(|state| state.lease_id.clone());
            generations.push(generation_view(
                occupant.as_deref(),
                dir,
                false,
                None,
                status,
            ));
        }
    }

    let unleased_candidates =
        unleased_candidates(&generations, statuses.iter().map(|(_, status)| status));
    DaemonView {
        router_dir: router_dir.to_path_buf(),
        registry,
        admission_owner,
        generations,
        unleased_candidates,
    }
}

struct Registered {
    lease_id: String,
    state_dir: PathBuf,
    drain_state: RollingDrainState,
}

/// One directory's status, or why it could not be read.
type StatusRead = std::result::Result<DaemonStatus, String>;

fn push_unique(directories: &mut Vec<PathBuf>, dir: &Path) {
    if !directories
        .iter()
        .any(|known| super::same_directory(known, dir))
    {
        directories.push(dir.to_path_buf());
    }
}

/// Build one generation's view from the status of its directory.
///
/// `lease_id` is the registered lease (or the directory's occupant for an
/// unregistered generation). When the directory holds a different lease, the
/// registered one was replaced in place and its process facts are unknown.
fn generation_view(
    lease_id: Option<&str>,
    state_dir: &Path,
    registered: bool,
    drain_state: Option<RollingDrainState>,
    status: &std::result::Result<DaemonStatus, String>,
) -> GenerationView {
    let unknown_process = ProcessObservation {
        pid: None,
        liveness: Liveness::Unknown,
    };
    let status = match status {
        Ok(status) => status,
        Err(reason) => {
            return GenerationView {
                lease_id: lease_id.map(str::to_string),
                state_dir: state_dir.to_path_buf(),
                registered,
                lifecycle: GenerationLifecycle::Unreadable {
                    reason: reason.clone(),
                },
                process: unknown_process,
                supervision: Supervision::Unknown,
                binary: BinaryFreshness::Unknown,
                jobs: Vec::new(),
            }
        }
    };
    let occupant = status.state.as_ref();
    let replaced_by = match (lease_id, occupant) {
        (Some(lease), Some(state)) if state.lease_id != lease => Some(state.lease_id.clone()),
        _ => None,
    };
    if let Some(current_lease_id) = replaced_by {
        return GenerationView {
            lease_id: lease_id.map(str::to_string),
            state_dir: state_dir.to_path_buf(),
            registered,
            lifecycle: GenerationLifecycle::Dead {
                proof: DeathProof::SameDirReplaced { current_lease_id },
            },
            process: unknown_process,
            supervision: Supervision::Unknown,
            binary: BinaryFreshness::Unknown,
            jobs: jobs_for(status, lease_id, false),
        };
    }

    let Some(state) = occupant else {
        let lifecycle = match status.freshness.stale_reason_code {
            Some(DaemonStaleReasonCode::LeaseCorrupt) => GenerationLifecycle::Unreadable {
                reason: status
                    .stale_reason
                    .clone()
                    .unwrap_or_else(|| "daemon lease is corrupt".to_string()),
            },
            _ => GenerationLifecycle::Stopped,
        };
        return GenerationView {
            lease_id: lease_id.map(str::to_string),
            state_dir: state_dir.to_path_buf(),
            registered,
            lifecycle,
            process: unknown_process,
            supervision: Supervision::Unknown,
            binary: BinaryFreshness::Unknown,
            jobs: jobs_for(status, lease_id, true),
        };
    };

    let liveness = process_liveness(state.pid);
    let lifecycle = if status.running {
        match drain_state {
            Some(RollingDrainState::Draining) => GenerationLifecycle::Draining,
            Some(RollingDrainState::Admitting) | None => GenerationLifecycle::Admitting,
        }
    } else {
        let terminated = status
            .termination_evidence
            .as_ref()
            .is_some_and(|evidence| evidence.lease_id.as_deref() == Some(state.lease_id.as_str()));
        GenerationLifecycle::Dead {
            proof: if terminated {
                DeathProof::TerminationEvidence
            } else {
                DeathProof::PidDead
            },
        }
    };
    let supervision = if state.startup_token.is_empty() {
        Supervision::External
    } else {
        Supervision::HomeboySupervised
    };
    let binary = if !status.running {
        BinaryFreshness::Unknown
    } else if executable_replaced(state.pid) {
        BinaryFreshness::Replaced
    } else if status.fresh {
        BinaryFreshness::Current
    } else {
        BinaryFreshness::Stale {
            reason_code: status.freshness.stale_reason_code,
        }
    };
    GenerationView {
        lease_id: Some(state.lease_id.clone()),
        state_dir: state_dir.to_path_buf(),
        registered,
        lifecycle,
        process: ProcessObservation {
            pid: Some(state.pid),
            liveness,
        },
        supervision,
        binary,
        jobs: jobs_for(status, Some(&state.lease_id), true),
    }
}

/// Jobs of one directory's store that belong to `lease_id`. The directory's
/// current occupant also takes jobs that record no lease, since the store is
/// its own.
fn jobs_for(status: &DaemonStatus, lease_id: Option<&str>, occupant: bool) -> Vec<JobCustodyView> {
    status
        .active_job_recovery_evidence
        .iter()
        .filter(|evidence| match evidence.daemon_lease_id.as_deref() {
            Some(owner) => Some(owner) == lease_id,
            None => occupant,
        })
        .map(|evidence| JobCustodyView {
            job_id: evidence.job_id,
            operation: evidence.operation.clone(),
            disposition: evidence.disposition,
            custody: job_custody(evidence),
        })
        .collect()
}

/// Map the job store's recovery disposition onto custody. Exhaustive on
/// purpose: a new disposition must decide its custody here.
pub fn job_custody(evidence: &DaemonActiveJobRecoveryEvidence) -> JobCustody {
    use DaemonActiveJobRecoveryDisposition as D;
    match evidence.disposition {
        D::TerminalEvidence => JobCustody::TerminalEvidence {
            status: evidence
                .terminal_evidence
                .or(evidence.linked_durable_run_terminal_status),
        },
        D::DriverRecovery => JobCustody::Resumable,
        // `MissingChildIdentityRecoverable` is only assigned while the job's
        // own lease is live: the daemon itself executes it.
        D::ProtectedLive | D::MissingChildIdentityRecoverable => JobCustody::Running,
        D::DeadChild | D::ReusedChildPid => JobCustody::Orphaned {
            proof: Some(DeathProof::PidDead),
        },
        D::BlockingAmbiguous => JobCustody::Orphaned { proof: None },
    }
}

fn process_liveness(pid: u32) -> Liveness {
    if crate::process::pid_is_running(pid) {
        Liveness::Live
    } else if homeboy_engine_primitives::command::process_is_zombie(pid) {
        Liveness::Zombie
    } else {
        Liveness::Dead
    }
}

/// Whether `pid` runs an executable that is no longer the file on disk.
/// Unreadable process metadata (another user's process) is not evidence.
fn executable_replaced(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|target| target.to_string_lossy().ends_with(" (deleted)"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

fn unleased_candidates<'a>(
    generations: &[GenerationView],
    statuses: impl Iterator<Item = &'a std::result::Result<DaemonStatus, String>>,
) -> Vec<UnleasedCandidate> {
    let leased_pids: BTreeSet<u32> = generations
        .iter()
        .filter_map(|generation| generation.process.pid)
        .collect();
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for status in statuses.flatten() {
        for candidate in &status.process_candidates {
            if candidate.ownership == DaemonProcessOwnership::Unrelated
                || leased_pids.contains(&candidate.pid)
                || !crate::process::pid_is_running(candidate.pid)
                || !seen.insert(candidate.pid)
            {
                continue;
            }
            candidates.push(UnleasedCandidate {
                pid: candidate.pid,
                ownership: candidate.ownership,
                bind_endpoint: candidate.bind_endpoint.clone(),
            });
        }
    }
    candidates
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
