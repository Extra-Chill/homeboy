//! Read existing custody metadata and observe owners in one place.
//!
//! Storage and phase-specific admission/deadline policies are unchanged. Missing
//! evidence is not proof of death; callers decide whether uncertainty fences a
//! phase or permits a deadline fallback.
use std::collections::BTreeSet;

use homeboy_core::process::{ProcessIdentityState, ProcessStartIdentity};
use serde_json::Value;
use uuid::Uuid;

use super::AgentTaskRunRecord;

pub(crate) enum RunOwner<'a> {
    Process {
        pid: u32,
        start_identity: ProcessEvidence,
    },
    /// Uses the same lease-scoped observation for every run in a reconcile pass.
    DaemonJob {
        job_id: Uuid,
        jobs: &'a SupervisorJobs,
    },
}

pub(crate) enum ProcessEvidence {
    PidOnly,
    StartIdentity(Option<ProcessStartIdentity>),
    ProviderStartTicks(Option<u64>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    Live,
    Dead,
    Unverifiable,
}

impl<'a> RunOwner<'a> {
    pub(crate) fn process(pid: u32) -> Self {
        Self::Process {
            pid,
            start_identity: ProcessEvidence::PidOnly,
        }
    }

    pub(crate) fn identified_process(pid: u32, start_identity: ProcessStartIdentity) -> Self {
        Self::Process {
            pid,
            start_identity: ProcessEvidence::StartIdentity(Some(start_identity)),
        }
    }

    /// Field names belong to the existing object's schema, not a role string.
    pub(crate) fn from_process_metadata(value: &Value, pid: &str, identity: &str) -> Option<Self> {
        Some(Self::Process {
            pid: u32::try_from(value[pid].as_u64()?).ok()?,
            start_identity: ProcessEvidence::StartIdentity(
                serde_json::from_value(value[identity].clone()).ok(),
            ),
        })
    }

    pub(crate) fn detached_child(value: &Value) -> Option<Self> {
        Self::from_process_metadata(value, "child_pid", "child_start_identity")
    }

    fn has_start_identity(&self) -> bool {
        matches!(
            self,
            Self::Process {
                start_identity: ProcessEvidence::StartIdentity(Some(_)),
                ..
            }
        )
    }

    pub(crate) fn live_pid(&self) -> Option<u32> {
        match self {
            Self::Process { pid, .. } if run_owner_liveness(self) == OwnerLiveness::Live => {
                Some(*pid)
            }
            _ => None,
        }
    }

    pub(crate) fn provider(value: &Value) -> Option<Self> {
        Some(Self::Process {
            pid: u32::try_from(value["owner_pid"].as_u64()?).ok()?,
            start_identity: ProcessEvidence::ProviderStartTicks(
                value["owner_linux_starttime_ticks"].as_u64(),
            ),
        })
    }

    pub(crate) fn daemon_job(value: &Value, jobs: &'a SupervisorJobs) -> Option<Self> {
        Some(Self::DaemonJob {
            job_id: Uuid::parse_str(value.as_str()?).ok()?,
            jobs,
        })
    }
}

/// The sole kernel/job-store observation for run custody. PID reuse means the
/// recorded owner is dead, even when a different process now occupies its PID.
pub(crate) fn run_owner_liveness(owner: &RunOwner) -> OwnerLiveness {
    match owner {
        RunOwner::Process { .. } => match process_observation(owner) {
            ProcessIdentityState::Live => OwnerLiveness::Live,
            ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch => {
                OwnerLiveness::Dead
            }
            ProcessIdentityState::Unverifiable => OwnerLiveness::Unverifiable,
        },
        RunOwner::DaemonJob { job_id, jobs } => match &jobs.live {
            Some(live) if live.contains(job_id) => OwnerLiveness::Live,
            Some(_) => OwnerLiveness::Dead,
            None => OwnerLiveness::Unverifiable,
        },
    }
}

/// An unavailable daemon or store is distinct from a successful snapshot
/// containing no live supervisor for a job.
#[derive(Default)]
pub(crate) struct SupervisorJobs {
    live: Option<BTreeSet<Uuid>>,
}

impl SupervisorJobs {
    pub(crate) fn for_record_in_roots(
        record: &AgentTaskRunRecord,
        roots: &homeboy_core::paths::PathRoots,
    ) -> Self {
        if record.metadata["local_cook_supervisor"]["job_type"].as_str()
            != Some(homeboy_core::daemon::step_job::STEP_JOB_TYPE)
        {
            return Self::default();
        }
        let path = homeboy_core::paths::observation_db_in_root(roots.data());
        let status = homeboy_core::daemon::read_status_at_paths(
            homeboy_core::paths::daemon_state_file_in_root(roots.config()),
            path.clone(),
        );
        Self::read_at(status, Ok(path))
    }

    #[cfg(test)]
    pub(crate) fn observed(live: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            live: Some(live.into_iter().collect()),
        }
    }

    pub(crate) fn read() -> Self {
        Self::read_at(
            homeboy_core::daemon::read_status(),
            homeboy_core::paths::observation_db(),
        )
    }

    fn read_at(
        status: homeboy_core::Result<homeboy_core::daemon::DaemonStatus>,
        path: homeboy_core::Result<std::path::PathBuf>,
    ) -> Self {
        use homeboy_core::api_jobs::{JobStatus, JobStore};
        let lease = status
            .ok()
            .filter(|daemon| daemon.reachable && daemon.fresh)
            .and_then(|daemon| daemon.state.map(|state| state.lease_id));
        let store = path
            .ok()
            .and_then(|path| JobStore::open_without_reconciliation(path).ok());
        let operation = ["controller.", homeboy_core::daemon::step_job::STEP_JOB_TYPE].concat();
        Self {
            live: lease.zip(store).map(|(lease, store)| {
                store
                    .list()
                    .into_iter()
                    .filter(|job| {
                        job.operation == operation
                            && job.daemon_lease_id.as_deref() == Some(lease.as_str())
                            && matches!(job.status, JobStatus::Queued | JobStatus::Running)
                    })
                    .map(|job| job.id)
                    .collect()
            }),
        }
    }
}

/// Detailed process evidence retained for diagnostic annotations and safe signalling.
/// All custody decisions use `run_owner_liveness`.
pub(crate) fn process_observation(owner: &RunOwner) -> ProcessIdentityState {
    let RunOwner::Process {
        pid,
        start_identity,
    } = owner
    else {
        return ProcessIdentityState::Unverifiable;
    };
    let (ticks, identity) = match start_identity {
        ProcessEvidence::PidOnly => {
            return if homeboy_core::process::pid_is_running(*pid) {
                ProcessIdentityState::Live
            } else {
                ProcessIdentityState::Dead
            };
        }
        ProcessEvidence::ProviderStartTicks(ticks) => (*ticks, None),
        ProcessEvidence::StartIdentity(identity) => (None, identity.as_ref()),
    };
    homeboy_core::process::process_identity_state_with_start_identity(*pid, ticks, identity)
}

/// Detached custody falls back to its lease only without usable identity.
pub(crate) fn detached_process_liveness(owner: Option<RunOwner>) -> Option<bool> {
    let owner = owner.filter(RunOwner::has_start_identity)?;
    match run_owner_liveness(&owner) {
        OwnerLiveness::Live => Some(true),
        OwnerLiveness::Dead => Some(false),
        OwnerLiveness::Unverifiable => None,
    }
}

pub(crate) fn continuation_owner_expired(value: &Value) -> bool {
    value["owner_pid"].as_u64().is_some_and(|pid| {
        u32::try_from(pid).map_or(true, |pid| {
            run_owner_liveness(&RunOwner::process(pid)) != OwnerLiveness::Live
        })
    })
}

pub(crate) fn has_live_execution_step_owner(record: &AgentTaskRunRecord) -> bool {
    use homeboy_lifecycle_contract::RunExecutionState;
    let scheduling = &record.metadata["execution_scheduling"];
    if !scheduling["current_intent_id"].is_string()
        || record.lifecycle.execution.state == RunExecutionState::Cancelled
    {
        return false;
    }
    if record.lifecycle.execution.state == RunExecutionState::Queued {
        return true;
    }
    let value = &record.metadata["execution_step_owner"];
    if value["intent_id"] != scheduling["current_intent_id"] || !value["job_id"].is_string() {
        return false;
    }
    RunOwner::from_process_metadata(value, "pid", "process_start_identity")
        .filter(RunOwner::has_start_identity)
        .is_none_or(|owner| run_owner_liveness(&owner) != OwnerLiveness::Dead)
}

pub(crate) fn local_owner_liveness(record: &AgentTaskRunRecord) -> super::LocalOwnerLiveness {
    use super::LocalOwnerLiveness;
    let Some(owner) = RunOwner::from_process_metadata(
        &record.metadata,
        "runner_pid",
        "runner_process_start_identity",
    ) else {
        return LocalOwnerLiveness::Absent;
    };
    match run_owner_liveness(&owner) {
        OwnerLiveness::Live => LocalOwnerLiveness::Live,
        OwnerLiveness::Dead => LocalOwnerLiveness::Dead,
        OwnerLiveness::Unverifiable => LocalOwnerLiveness::Unverifiable,
    }
}

/// A runner takes precedence; cancellation requires exact start evidence.
pub(crate) fn local_cancellable_owner(
    record: &AgentTaskRunRecord,
) -> Option<(u32, ProcessStartIdentity)> {
    let owner = if record.metadata.get("runner_pid").is_some() {
        RunOwner::from_process_metadata(
            &record.metadata,
            "runner_pid",
            "runner_process_start_identity",
        )?
    } else {
        let value = &record.metadata["execution_step_owner"];
        let intent = &record.metadata["execution_scheduling"]["current_intent_id"];
        if !intent.is_string() || value["intent_id"] != *intent {
            return None;
        }
        RunOwner::from_process_metadata(value, "pid", "process_start_identity")?
    };
    let RunOwner::Process {
        pid,
        start_identity: ProcessEvidence::StartIdentity(Some(ref identity)),
        ..
    } = owner
    else {
        return None;
    };
    if pid == std::process::id() || run_owner_liveness(&owner) != OwnerLiveness::Live {
        return None;
    }
    Some((pid, identity.clone()))
}
