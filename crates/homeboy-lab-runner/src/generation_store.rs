use std::path::PathBuf;

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::{RollingGenerations, RunnerSession};

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
