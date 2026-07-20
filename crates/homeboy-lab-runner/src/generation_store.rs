use std::path::PathBuf;
use std::time::Duration;

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::{RollingGenerations, RunnerDaemonGenerationStatus, RunnerSession};

fn path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("generations.json"))
}

fn legacy_generation(session: &RunnerSession) -> String {
    session
        .remote_daemon_lease_id
        .clone()
        .unwrap_or_else(|| "legacy".to_string())
}

pub(crate) fn read(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Option<RollingGenerations<RunnerSession>>> {
    let path = path(runner_id)?;
    if !path.exists() {
        return Ok(legacy
            .map(|session| RollingGenerations::new(legacy_generation(session), session.clone())));
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

pub(crate) fn write(
    runner_id: &str,
    generations: &RollingGenerations<RunnerSession>,
) -> Result<()> {
    homeboy_core::engine::local_files::write_json_file(&path(runner_id)?, generations)
}

pub(crate) fn admission_session(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Option<RunnerSession>> {
    Ok(read(runner_id, legacy)?.and_then(|generations| {
        generations
            .generations
            .get(&generations.admission_owner)
            .map(|generation| generation.endpoint.clone())
    }))
}

pub(crate) fn job_session(
    runner_id: &str,
    job_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Option<RunnerSession>> {
    Ok(read(runner_id, legacy)?.and_then(|generations| {
        generations.job_owner(job_id).and_then(|owner| {
            generations
                .generations
                .get(owner)
                .map(|generation| generation.endpoint.clone())
        })
    }))
}

pub(crate) fn status_projection(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Vec<RunnerDaemonGenerationStatus>> {
    Ok(
        read(runner_id, legacy)?.map_or_else(Vec::new, |generations| {
            generations
                .generations
                .into_iter()
                .map(|(generation, entry)| RunnerDaemonGenerationStatus {
                    admission_owner: generation == generations.admission_owner,
                    generation,
                    drain_state: entry.drain_state,
                    active_job_count: entry.active_jobs,
                    homeboy_build_identity: entry.endpoint.homeboy_build_identity,
                    remote_daemon_lease_id: entry.endpoint.remote_daemon_lease_id,
                    remote_daemon_address: entry.endpoint.remote_daemon_address,
                    local_url: entry.endpoint.local_url,
                })
                .collect()
        }),
    )
}

/// Reconciliation is intentionally fail-closed: an unreachable draining
/// endpoint remains recorded and routable. Only its own health response may
/// authorize the zero-job lease-bound stop.
pub(crate) fn reconcile(runner_id: &str, legacy: Option<&RunnerSession>) -> Result<()> {
    let Some(mut generations) = read(runner_id, legacy)? else {
        return Ok(());
    };
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build generation reconcile client: {error}"))
        })?;
    let drained = generations
        .generations
        .iter()
        .filter_map(|(generation, entry)| {
            (entry.drain_state == crate::RollingDrainState::Draining)
                .then_some((generation.clone(), entry.endpoint.clone()))
        })
        .filter_map(|(generation, session)| {
            let local_url = session.local_url.clone()?;
            let health = client
                .get(format!("{}/health", local_url.trim_end_matches('/')))
                .send()
                .ok()?;
            let body: serde_json::Value = health.json().ok()?;
            let active = body
                .pointer("/freshness/active_jobs")
                .and_then(serde_json::Value::as_u64)?;
            (active == 0).then_some((generation, session, local_url))
        })
        .collect::<Vec<_>>();
    for (generation, session, local_url) in drained {
        let Some(lease_id) = session.remote_daemon_lease_id.as_deref() else {
            continue;
        };
        let stopped = client
            .post(format!(
                "{}/lifecycle/stop",
                local_url.trim_end_matches('/')
            ))
            .json(&serde_json::json!({ "lease_id": lease_id, "force": false }))
            .send()
            .is_ok_and(|response| response.status().is_success());
        if stopped {
            if let Some(pid) = session.tunnel_pid {
                crate::connection::terminate_generation_tunnel(pid);
            }
            generations.generations.remove(&generation);
            generations
                .job_owners
                .retain(|_, owner| owner != &generation);
        }
    }
    write(runner_id, &generations)
}

pub(crate) fn record_job(runner_id: &str, session: &RunnerSession, job_id: &str) -> Result<()> {
    let mut generations = read(runner_id, Some(session))?
        .unwrap_or_else(|| RollingGenerations::new(legacy_generation(session), session.clone()));
    generations.admit_job(job_id);
    write(runner_id, &generations)
}

pub(crate) fn activate(
    runner_id: &str,
    current: &RunnerSession,
    generation: String,
    candidate: RunnerSession,
    draining_job_ids: &[String],
) -> Result<()> {
    let mut generations = read(runner_id, Some(current))?
        .unwrap_or_else(|| RollingGenerations::new(legacy_generation(current), current.clone()));
    generations.begin(generation.clone(), candidate);
    generations.activate(&generation);
    let draining_owner = legacy_generation(current);
    for job_id in draining_job_ids {
        if generations
            .job_owners
            .insert(job_id.clone(), draining_owner.clone())
            .is_none()
        {
            if let Some(draining) = generations.generations.get_mut(&draining_owner) {
                draining.active_jobs += 1;
            }
        }
    }
    write(runner_id, &generations)
}
