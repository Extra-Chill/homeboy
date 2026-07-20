use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::RunnerSession;

const SCHEMA: &str = "homeboy/runner-daemon-generations/v1";

/// Controller-local routing state. The active generation receives admissions;
/// draining generations remain addressable only for jobs accepted by that lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RunnerDaemonGenerations {
    schema: String,
    active_lease_id: String,
    generations: BTreeMap<String, RunnerSession>,
    #[serde(default)]
    job_leases: BTreeMap<String, String>,
}

fn path(runner_id: &str) -> Result<std::path::PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("daemon-generations.json"))
}

fn load(runner_id: &str) -> Result<Option<RunnerDaemonGenerations>> {
    let path = path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let router = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    Ok(Some(router))
}

fn save(router: &RunnerDaemonGenerations, runner_id: &str) -> Result<()> {
    homeboy_core::engine::local_files::write_json_file(&path(runner_id)?, router)
}

fn lease(session: &RunnerSession) -> Result<&str> {
    session
        .remote_daemon_lease_id
        .as_deref()
        .filter(|lease| !lease.is_empty())
        .ok_or_else(|| {
            Error::internal_unexpected("direct runner daemon generation has no lease identity")
        })
}

pub(crate) fn activate(
    runner_id: &str,
    previous: RunnerSession,
    next: RunnerSession,
) -> Result<()> {
    let previous_lease = lease(&previous)?.to_string();
    let next_lease = lease(&next)?.to_string();
    if previous_lease == next_lease {
        return Ok(());
    }
    let mut generations = load(runner_id)?.unwrap_or(RunnerDaemonGenerations {
        schema: SCHEMA.to_string(),
        active_lease_id: previous_lease.clone(),
        generations: BTreeMap::new(),
        job_leases: BTreeMap::new(),
    });
    generations.generations.insert(previous_lease, previous);
    generations.generations.insert(next_lease.clone(), next);
    generations.active_lease_id = next_lease;
    save(&generations, runner_id)
}

pub(crate) fn record_job(runner_id: &str, job_id: &str, session: &RunnerSession) -> Result<()> {
    let Some(mut generations) = load(runner_id)? else {
        return Ok(());
    };
    let lease = lease(session)?.to_string();
    if !generations.generations.contains_key(&lease) {
        return Ok(());
    }
    generations.job_leases.insert(job_id.to_string(), lease);
    save(&generations, runner_id)
}

pub(crate) fn session_for_job(runner_id: &str, job_id: &str) -> Result<Option<RunnerSession>> {
    let Some(generations) = load(runner_id)? else {
        return Ok(None);
    };
    Ok(generations
        .job_leases
        .get(job_id)
        .and_then(|lease| generations.generations.get(lease))
        .cloned())
}

pub(crate) fn sessions(runner_id: &str) -> Result<Vec<RunnerSession>> {
    Ok(load(runner_id)?
        .map(|router| router.generations.into_values().collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerSessionRole, RunnerTunnelMode};

    fn session(lease: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:1".to_string()),
            local_port: Some(1),
            local_url: Some("http://127.0.0.1:1".to_string()),
            tunnel_pid: None,
            remote_daemon_pid: Some(1),
            remote_daemon_lease_id: Some(lease.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(format!("homeboy test+{lease}")),
            connected_at: "now".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn accepted_jobs_stay_routed_to_their_draining_generation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let a = session("lease-a");
            let b = session("lease-b");
            activate("lab", a.clone(), b.clone()).expect("activate B");
            record_job("lab", "job-a", &a).expect("bind A job");
            record_job("lab", "job-b", &b).expect("bind B job");
            assert_eq!(session_for_job("lab", "job-a").unwrap(), Some(a));
            assert_eq!(session_for_job("lab", "job-b").unwrap(), Some(b));
        });
    }
}
