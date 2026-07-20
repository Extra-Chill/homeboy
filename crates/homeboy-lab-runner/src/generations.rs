use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::{connection, RunnerSession};

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

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDaemonGenerationStatus {
    pub lease_id: String,
    pub state: &'static str,
    pub active_jobs: Option<usize>,
    pub session: RunnerSession,
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

/// Restore the previous persisted router when a later cutover step fails.
pub(crate) fn rollback_activation(
    runner_id: &str,
    previous: &RunnerSession,
    next: &RunnerSession,
) -> Result<()> {
    let Some(mut generations) = load(runner_id)? else {
        return Ok(());
    };
    let previous_lease = lease(previous)?.to_string();
    let next_lease = lease(next)?.to_string();
    generations.generations.remove(&next_lease);
    generations
        .generations
        .insert(previous_lease.clone(), previous.clone());
    generations.active_lease_id = previous_lease;
    generations
        .job_leases
        .retain(|_, lease| lease != &next_lease);
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

/// Inspect every persisted generation. This is intentionally observational:
/// status polling must never own a generation's retirement.
pub fn inventory(runner_id: &str) -> Result<Vec<RunnerDaemonGenerationStatus>> {
    let Some(generations) = load(runner_id)? else {
        return Ok(Vec::new());
    };
    Ok(generations
        .generations
        .iter()
        .map(|(lease_id, session)| RunnerDaemonGenerationStatus {
            lease_id: lease_id.clone(),
            state: if lease_id == &generations.active_lease_id {
                "accepting"
            } else {
                "draining"
            },
            active_jobs: connection::generation_active_jobs(session).ok(),
            session: session.clone(),
        })
        .collect())
}

/// Reconcile draining generations after a durable terminal-job transition.
/// The daemon's active-job count is authoritative; controller routing records
/// only decide which generations are eligible to be retired.
pub(crate) fn reconcile_terminal_job(runner_id: &str) -> Result<()> {
    reconcile_draining_generations_with(
        runner_id,
        connection::generation_active_jobs,
        connection::retire_direct_generation,
    )
}

fn reconcile_draining_generations_with<Count, Retire>(
    runner_id: &str,
    mut active_jobs: Count,
    mut retire: Retire,
) -> Result<()>
where
    Count: FnMut(&RunnerSession) -> Result<usize>,
    Retire: FnMut(&RunnerSession) -> Result<()>,
{
    let Some(mut generations) = load(runner_id)? else {
        return Ok(());
    };
    let draining = generations
        .generations
        .iter()
        .filter(|(lease_id, _)| *lease_id != &generations.active_lease_id)
        .map(|(lease_id, session)| (lease_id.clone(), session.clone()))
        .collect::<Vec<_>>();
    for (lease_id, session) in draining {
        if active_jobs(&session)? != 0 {
            continue;
        }
        retire(&session)?;
        generations.generations.remove(&lease_id);
        generations
            .job_leases
            .retain(|_, assigned| assigned != &lease_id);
    }
    save(&generations, runner_id)
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

    #[test]
    fn failed_cutover_restores_a_and_removes_b_routes() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let a = session("lease-a");
            let b = session("lease-b");
            activate("lab", a.clone(), b.clone()).expect("activate B");
            record_job("lab", "job-b", &b).expect("bind B job");
            rollback_activation("lab", &a, &b).expect("rollback B");
            assert_eq!(session_for_job("lab", "job-b").unwrap(), None);
            assert_eq!(sessions("lab").unwrap(), vec![a]);
        });
    }

    #[test]
    fn terminal_reconciliation_retires_only_authoritative_zero_draining_generation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let a = session("lease-a");
            let b = session("lease-b");
            activate("lab", a.clone(), b.clone()).expect("activate B");
            let mut retired = Vec::new();
            reconcile_draining_generations_with(
                "lab",
                |session| {
                    Ok(
                        if session.remote_daemon_lease_id.as_deref() == Some("lease-a") {
                            1
                        } else {
                            0
                        },
                    )
                },
                |session| {
                    retired.push(session.remote_daemon_lease_id.clone().expect("lease"));
                    Ok(())
                },
            )
            .expect("reconcile active A");
            assert!(retired.is_empty());
            assert_eq!(session_for_job("lab", "job-a").unwrap(), None);
            reconcile_draining_generations_with(
                "lab",
                |_| Ok(0),
                |session| {
                    retired.push(session.remote_daemon_lease_id.clone().expect("lease"));
                    Ok(())
                },
            )
            .expect("reconcile terminal A");
            assert_eq!(retired, vec!["lease-a"]);
            assert_eq!(sessions("lab").unwrap(), vec![b]);
        });
    }

    #[test]
    fn durable_router_survives_controller_reload_and_admits_b() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let a = session("lease-a");
            let b = session("lease-b");
            activate("lab", a.clone(), b.clone()).expect("activate B");
            record_job("lab", "job-a", &a).expect("bind A job");
            record_job("lab", "job-b", &b).expect("bind B job");
            let raw =
                std::fs::read_to_string(path("lab").expect("router path")).expect("router exists");
            let reloaded: RunnerDaemonGenerations =
                serde_json::from_str(&raw).expect("reload router");
            assert_eq!(reloaded.active_lease_id, "lease-b");
            assert_eq!(session_for_job("lab", "job-a").unwrap(), Some(a));
            assert_eq!(session_for_job("lab", "job-b").unwrap(), Some(b));
        });
    }

    #[test]
    fn inventory_labels_b_accepting_and_a_draining_without_retiring_on_poll() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let a = session("lease-a");
            let b = session("lease-b");
            activate("lab", a.clone(), b.clone()).expect("activate B");
            let inventory = inventory("lab").expect("status inventory");
            assert_eq!(inventory[0].state, "draining");
            assert_eq!(inventory[1].state, "accepting");
            assert_eq!(sessions("lab").unwrap(), vec![a, b]);
        });
    }
}
