use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::Duration;

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::rolling_generation::RollingResultOwnerRetirement;
use crate::{RollingGenerations, RunnerDaemonGenerationStatus, RunnerSession};

/// The durable generation registry is scoped to one runner. Keep this wrapper
/// at the state-store boundary so rolling ownership remains reusable in memory.
#[derive(serde::Serialize, serde::Deserialize)]
struct GenerationRegistry<E> {
    runner_id: String,
    #[serde(flatten)]
    generations: RollingGenerations<E>,
}

fn path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("generations.json"))
}

/// Serialize read-modify-write registry updates independently for each runner.
/// A status reconciliation and `/exec` acceptance can arrive concurrently.
fn with_registry_lock<T>(runner_id: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = path(runner_id)?.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create generation lock directory".to_string()),
            )
        })?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("open generation lock".to_string()))
        })?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock generation registry".to_string()),
            ));
        }
    }
    let result = operation();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    }
    result
}

fn legacy_generation(session: &RunnerSession) -> String {
    session
        .remote_daemon_lease_id
        .clone()
        .unwrap_or_else(|| "legacy".to_string())
}

#[cfg(test)]
const LEGACY_MIGRATION_SYNC_DIR_ENV: &str = "HOMEBOY_LEGACY_GENERATION_MIGRATION_SYNC_DIR";

#[cfg(test)]
const AUTHENTICATED_ADMISSION_SYNC_DIR_ENV: &str = "HOMEBOY_AUTHENTICATED_ADMISSION_SYNC_DIR";

#[cfg(test)]
fn pause_legacy_migration_before_lock() {
    let Some(sync_dir) = std::env::var_os(LEGACY_MIGRATION_SYNC_DIR_ENV) else {
        return;
    };
    let sync_dir = PathBuf::from(sync_dir);
    std::fs::write(sync_dir.join("migration-ready"), "ready").expect("signal legacy migration");
    while !sync_dir.join("allow-migration").exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
fn pause_authenticated_admission_after_read() {
    let Some(sync_dir) = std::env::var_os(AUTHENTICATED_ADMISSION_SYNC_DIR_ENV) else {
        return;
    };
    let sync_dir = PathBuf::from(sync_dir);
    std::fs::write(sync_dir.join("admission-read"), "ready").expect("signal admission read");
    while !sync_dir.join("allow-admission").exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    validate_registry_shape(&value)?;
    match serde_json::from_str::<GenerationRegistry<RunnerSession>>(&raw) {
        Ok(registry) if registry.runner_id == runner_id => Ok(Some(registry.generations)),
        Ok(registry) => Err(Error::config_invalid_value(
            "runner_id",
            Some(registry.runner_id),
            format!("generation registry must match runner-scoped path `{runner_id}`"),
        )),
        Err(error) => recover_missing_runner_id(runner_id, &path, &raw, error),
    }
}

/// Read while the caller already holds this runner's registry lock. Legacy
/// migration stays inside that transaction instead of attempting a re-entrant
/// `flock` acquisition.
fn read_locked(
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
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    validate_registry_shape(&value)?;
    match serde_json::from_str::<GenerationRegistry<RunnerSession>>(&raw) {
        Ok(registry) if registry.runner_id == runner_id => Ok(Some(registry.generations)),
        Ok(registry) => Err(Error::config_invalid_value(
            "runner_id",
            Some(registry.runner_id),
            format!("generation registry must match runner-scoped path `{runner_id}`"),
        )),
        Err(error) => recover_missing_runner_id_unlocked(runner_id, &path, &raw, error),
    }
}

fn validate_registry_shape(value: &serde_json::Value) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Err(Error::config_invalid_value(
            "generation_registry",
            None,
            "generation registry must be a JSON object",
        ));
    };
    if let Some(field) = object.keys().find(|key| {
        !matches!(
            key.as_str(),
            "runner_id"
                | "admission_owner"
                | "generations"
                | "job_owners"
                | "run_owners"
                | "artifact_owners"
        )
    }) {
        return Err(Error::config_invalid_value(
            format!("generation_registry.{field}"),
            None,
            "generation registry has an unsupported top-level field",
        ));
    }
    Ok(())
}

pub(crate) fn write(
    runner_id: &str,
    generations: &RollingGenerations<RunnerSession>,
) -> Result<()> {
    homeboy_core::engine::local_files::write_json_file(
        &path(runner_id)?,
        &GenerationRegistry {
            runner_id: runner_id.to_string(),
            generations: generations.clone(),
        },
    )
}

/// A prior writer omitted the registry's runner identity. Repair only that
/// exact shape when every persisted endpoint independently confirms this path.
/// The normal atomic writer retains every ownership map during the rewrite.
fn recover_missing_runner_id(
    runner_id: &str,
    _path: &std::path::Path,
    _raw: &str,
    deserialize_error: serde_json::Error,
) -> Result<Option<RollingGenerations<RunnerSession>>> {
    #[cfg(test)]
    pause_legacy_migration_before_lock();

    // Migration is a registry mutation too. Reload after acquiring the same
    // runner lock used by job admission so an older legacy snapshot cannot
    // overwrite ownership accepted by a concurrent controller.
    with_registry_lock(runner_id, || {
        let path = path(runner_id)?;
        let raw = std::fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        validate_registry_shape(&value)?;
        match serde_json::from_str::<GenerationRegistry<RunnerSession>>(&raw) {
            Ok(registry) if registry.runner_id == runner_id => Ok(Some(registry.generations)),
            Ok(registry) => Err(Error::config_invalid_value(
                "runner_id",
                Some(registry.runner_id),
                format!("generation registry must match runner-scoped path `{runner_id}`"),
            )),
            Err(current_error) => recover_missing_runner_id_unlocked(
                runner_id,
                &path,
                &raw,
                if value.get("runner_id").is_some() {
                    current_error
                } else {
                    deserialize_error
                },
            ),
        }
    })
}

fn recover_missing_runner_id_unlocked(
    runner_id: &str,
    path: &std::path::Path,
    raw: &str,
    deserialize_error: serde_json::Error,
) -> Result<Option<RollingGenerations<RunnerSession>>> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if value.get("runner_id").is_some() {
        return Err(Error::config_invalid_json(
            path.display().to_string(),
            deserialize_error,
        ));
    }
    let generations = serde_json::from_value::<RollingGenerations<RunnerSession>>(value)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if runner_id.trim().is_empty() {
        return Err(Error::config_invalid_value(
            "runner_id",
            Some(runner_id.to_string()),
            "runner-scoped generation registry path has an empty runner ID",
        ));
    }
    if generations.generations.is_empty() {
        return Err(Error::config_invalid_value(
            "generations",
            Some("{}".to_string()),
            "prior generation registry has no endpoints to establish its runner identity",
        ));
    }
    for (generation, entry) in &generations.generations {
        let endpoint_runner_id = &entry.endpoint.runner_id;
        if endpoint_runner_id.trim().is_empty() {
            return Err(Error::config_invalid_value(
                format!("generations.{generation}.endpoint.runner_id"),
                Some(endpoint_runner_id.clone()),
                format!("prior generation `{generation}` has an empty endpoint runner ID"),
            ));
        }
        if endpoint_runner_id != runner_id {
            return Err(Error::config_invalid_value(
                format!("generations.{generation}.endpoint.runner_id"),
                Some(endpoint_runner_id.clone()),
                format!(
                    "prior generation `{generation}` endpoint runner ID does not match runner-scoped path `{runner_id}`"
                ),
            ));
        }
    }
    write(runner_id, &generations)?;
    Ok(Some(generations))
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

/// Promote the direct session that status has just health-checked. The
/// controller session is the admission authority for new work; older entries
/// remain draining so their owned jobs keep their original endpoints.
pub(crate) fn reconcile_admission_session(runner_id: &str, session: &RunnerSession) -> Result<()> {
    if session.mode != crate::RunnerTunnelMode::DirectSsh {
        return Ok(());
    }
    let Some(lease_id) = session
        .remote_daemon_lease_id
        .as_deref()
        .filter(|lease_id| !lease_id.is_empty())
    else {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "runner `{runner_id}` has a healthy direct daemon tunnel without a lease; refusing unbound admission"
            ),
            Some(runner_id.to_string()),
            None,
        ));
    };
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(session))? else {
            return Ok(());
        };
        let generation = generations
            .generations
            .iter()
            .find_map(|(generation, entry)| {
                (entry.endpoint.remote_daemon_lease_id.as_deref() == Some(lease_id))
                    .then_some(generation.clone())
            })
            .unwrap_or_else(|| lease_id.to_string());
        if !generations.generations.contains_key(&generation) {
            generations.begin(generation.clone(), session.clone());
        }
        generations.activate(&generation);
        generations
            .generations
            .get_mut(&generation)
            .expect("generation was inserted or found")
            .endpoint = session.clone();
        write(runner_id, &generations)
    })
}

pub(crate) fn job_session(
    runner_id: &str,
    job_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Option<RunnerSession>> {
    Ok(read(runner_id, legacy)?.and_then(|generations| {
        generations
            .job_owner(job_id)
            .and_then(|owner| owner_session(&generations, owner))
    }))
}

fn owner_session(
    generations: &RollingGenerations<RunnerSession>,
    owner: &str,
) -> Option<RunnerSession> {
    generations
        .generations
        .get(owner)
        .or_else(|| {
            generations.generations.values().find(|generation| {
                generation.endpoint.remote_daemon_lease_id.as_deref() == Some(owner)
            })
        })
        .map(|generation| generation.endpoint.clone())
}

/// Resolve every generation-aware endpoint through one persisted ownership
/// ledger. Callers supply whichever lifecycle identity they have; job ownership
/// wins because it is the original admission authority.
pub(crate) fn endpoint_session(
    runner_id: &str,
    job_id: Option<&str>,
    run_id: Option<&str>,
    artifact_id: Option<&str>,
    legacy: Option<&RunnerSession>,
) -> Result<Option<RunnerSession>> {
    Ok(read(runner_id, legacy)?.and_then(|generations| {
        generations
            .endpoint_owner(job_id, run_id, artifact_id)
            .and_then(|owner| owner_session(&generations, owner))
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
                    job_owner_count: generations
                        .job_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, &generation, &entry.endpoint)
                        })
                        .count(),
                    run_owner_count: generations
                        .run_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, &generation, &entry.endpoint)
                        })
                        .count(),
                    artifact_owner_count: generations
                        .artifact_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, &generation, &entry.endpoint)
                        })
                        .count(),
                    admission_owner: generation == generations.admission_owner,
                    generation,
                    drain_state: entry.drain_state,
                    active_job_count: entry.active_jobs,
                    observed_active_job_count: entry.observed_active_jobs,
                    active_job_count_authoritative: entry.observed_active_jobs.is_some(),
                    homeboy_build_identity: entry.endpoint.homeboy_build_identity,
                    remote_daemon_lease_id: entry.endpoint.remote_daemon_lease_id,
                    remote_daemon_address: entry.endpoint.remote_daemon_address,
                    local_url: entry.endpoint.local_url,
                })
                .collect()
        }),
    )
}

/// Report whether a refresh must preserve existing daemon endpoints. Current
/// daemon activity is queried separately; this covers durable ownership that
/// remains after a producing job has completed.
pub(crate) fn requires_generation_preserving_refresh(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<bool> {
    Ok(read(runner_id, legacy)?.is_some_and(|generations| {
        generations
            .generations
            .values()
            .any(|generation| generation.active_jobs > 0)
            || !generations.job_owners.is_empty()
            || !generations.run_owners.is_empty()
            || !generations.artifact_owners.is_empty()
    }))
}

fn owner_matches_generation(owner: &str, generation: &str, endpoint: &RunnerSession) -> bool {
    owner == generation || endpoint.remote_daemon_lease_id.as_deref() == Some(owner)
}

fn job_owner_ids_for(
    generations: &RollingGenerations<RunnerSession>,
    generation: &str,
    endpoint: &RunnerSession,
) -> Vec<String> {
    generations
        .job_owners
        .iter()
        .filter_map(|(job_id, owner)| {
            owner_matches_generation(owner, generation, endpoint).then(|| job_id.clone())
        })
        .collect()
}

fn has_result_owners_for(
    generations: &RollingGenerations<RunnerSession>,
    generation: &str,
    endpoint: &RunnerSession,
) -> bool {
    generations
        .run_owners
        .values()
        .chain(generations.artifact_owners.values())
        .any(|owner| owner_matches_generation(owner, generation, endpoint))
}

pub(crate) fn live_sessions(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Vec<RunnerSession>> {
    Ok(
        read(runner_id, legacy)?.map_or_else(Vec::new, |generations| {
            generations
                .generations
                .into_values()
                .map(|generation| generation.endpoint)
                .collect()
        }),
    )
}

pub(crate) fn clear(runner_id: &str) -> Result<()> {
    with_registry_lock(runner_id, || {
        let path = path(runner_id)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("delete {}", path.display())),
                )
            })?;
        }
        Ok(())
    })
}

trait GenerationEndpointOperations {
    /// Terminal linked handoffs can survive a controller restart in a daemon's
    /// job store. Settle them before treating its active count as ownership.
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool;
    fn active_jobs(&self, session: &RunnerSession) -> Option<usize>;
    fn stop(&self, session: &RunnerSession) -> bool;
    fn terminate_tunnel(&self, pid: u32);
}

struct HttpGenerationEndpointOperations {
    client: reqwest::blocking::Client,
}

impl GenerationEndpointOperations for HttpGenerationEndpointOperations {
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
        let Some(local_url) = session.local_url.as_deref() else {
            return false;
        };
        self.client
            .post(format!(
                "{}/jobs/reconcile-terminal",
                local_url.trim_end_matches('/')
            ))
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    fn active_jobs(&self, session: &RunnerSession) -> Option<usize> {
        let local_url = session.local_url.as_deref()?;
        let health = self
            .client
            .get(format!("{}/health", local_url.trim_end_matches('/')))
            .send()
            .ok()?;
        health
            .json::<serde_json::Value>()
            .ok()?
            .pointer("/freshness/active_jobs")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
    }

    fn stop(&self, session: &RunnerSession) -> bool {
        let (Some(local_url), Some(lease_id)) = (
            session.local_url.as_deref(),
            session.remote_daemon_lease_id.as_deref(),
        ) else {
            return false;
        };
        self.client
            .post(format!(
                "{}/lifecycle/stop",
                local_url.trim_end_matches('/')
            ))
            .json(&serde_json::json!({ "lease_id": lease_id, "force": false }))
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    fn terminate_tunnel(&self, pid: u32) {
        crate::connection::terminate_generation_tunnel(pid);
    }
}

/// Reconciliation is intentionally fail-closed: an unreachable draining
/// endpoint remains recorded and routable. Each reachable draining daemon first
/// settles terminal durable handoffs, then its own health response becomes the
/// authoritative count for that generation.
pub(crate) fn reconcile(runner_id: &str, legacy: Option<&RunnerSession>) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build generation reconcile client: {error}"))
        })?;
    reconcile_with(
        runner_id,
        legacy,
        &HttpGenerationEndpointOperations { client },
    )
}

fn reconcile_with(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    operations: &impl GenerationEndpointOperations,
) -> Result<()> {
    let Some(generations) = read(runner_id, legacy)? else {
        return Ok(());
    };
    let observations = generations
        .generations
        .iter()
        .map(|(generation, entry)| {
            (
                generation.clone(),
                entry.endpoint.clone(),
                entry.active_jobs,
                job_owner_ids_for(&generations, generation, &entry.endpoint),
                entry.drain_state == crate::RollingDrainState::Draining,
            )
        })
        .map(
            |(generation, session, active_jobs, job_owner_ids, draining)| {
                let observed_active_jobs =
                    if draining && !operations.reconcile_terminal_jobs(&session) {
                        None
                    } else {
                        operations.active_jobs(&session)
                    };
                (generation, active_jobs, job_owner_ids, observed_active_jobs)
            },
        )
        .collect::<Vec<_>>();
    // Persist every bounded remote observation under the registry lock. The
    // endpoint stop remains outside this lock because it is remote I/O.
    let stoppable = with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, legacy)? else {
            return Ok(Vec::new());
        };
        let mut authoritative_zero = Vec::new();
        for (generation, prior_active_jobs, prior_job_owner_ids, observed_active_jobs) in
            &observations
        {
            if let Some(entry) = generations.generations.get(generation) {
                let state_unchanged = entry.active_jobs == *prior_active_jobs
                    && job_owner_ids_for(&generations, generation, &entry.endpoint)
                        == *prior_job_owner_ids;
                if !state_unchanged {
                    generations
                        .generations
                        .get_mut(generation)
                        .expect("generation was checked")
                        .observed_active_jobs = None;
                    continue;
                }
            }
            if let Some(entry) = generations.generations.get_mut(generation) {
                entry.observed_active_jobs = *observed_active_jobs;
                if let Some(active_jobs) = observed_active_jobs {
                    entry.active_jobs = *active_jobs;
                }
                if entry.drain_state == crate::RollingDrainState::Draining
                    && observed_active_jobs == &Some(0)
                {
                    generations.job_owners.retain(|_, owner| {
                        !owner_matches_generation(owner, generation, &entry.endpoint)
                    });
                    authoritative_zero.push(generation.clone());
                }
            }
        }
        let stoppable = authoritative_zero
            .iter()
            .filter_map(|generation| {
                generations.generations.get(generation).and_then(|entry| {
                    (entry.drain_state == crate::RollingDrainState::Draining
                        && entry.active_jobs == 0
                        && !has_result_owners_for(&generations, generation, &entry.endpoint))
                    .then_some((generation.clone(), entry.endpoint.clone()))
                })
            })
            .collect::<Vec<_>>();
        write(runner_id, &generations)?;
        Ok(stoppable)
    })?;

    let stopped = stoppable
        .into_iter()
        .filter_map(|(generation, session)| {
            if operations.stop(&session) {
                if let Some(pid) = session.tunnel_pid {
                    operations.terminate_tunnel(pid);
                }
                Some(generation)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, legacy)? else {
            return Ok(());
        };
        for generation in &stopped {
            let should_remove = generations
                .generations
                .get(generation)
                .is_some_and(|entry| {
                    entry.drain_state == crate::RollingDrainState::Draining
                        && entry.active_jobs == 0
                        && !has_result_owners_for(&generations, generation, &entry.endpoint)
                });
            if should_remove {
                let endpoint = generations
                    .generations
                    .remove(generation)
                    .expect("generation was checked")
                    .endpoint;
                generations
                    .job_owners
                    .retain(|_, owner| !owner_matches_generation(owner, generation, &endpoint));
            }
        }
        write(runner_id, &generations)
    })
}

pub(crate) fn record_job(runner_id: &str, session: &RunnerSession, job_id: &str) -> Result<()> {
    with_registry_lock(runner_id, || {
        let mut generations = read_locked(runner_id, Some(session))?.unwrap_or_else(|| {
            RollingGenerations::new(legacy_generation(session), session.clone())
        });
        let owner = session
            .remote_daemon_lease_id
            .as_deref()
            .and_then(|lease_id| {
                generations
                    .generations
                    .iter()
                    .find_map(|(generation, entry)| {
                        (entry.endpoint.remote_daemon_lease_id.as_deref() == Some(lease_id))
                            .then_some(generation.clone())
                    })
            })
            .unwrap_or_else(|| generations.admission_owner.clone());
        generations.admit_job_for(&owner, job_id);
        write(runner_id, &generations)
    })
}

/// Persist a daemon that has already passed the authenticated connection and
/// endpoint identity checks as the admission owner. This survives controller
/// session-file drift, while retaining older generations for any work pinned
/// to them.
pub(crate) fn record_authenticated_admission(
    runner_id: &str,
    session: &RunnerSession,
) -> Result<()> {
    let generation = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        Error::internal_unexpected("authenticated daemon session has no lease ID")
    })?;
    with_registry_lock(runner_id, || {
        let mut generations = read_locked(runner_id, None)?
            .unwrap_or_else(|| RollingGenerations::new(generation, session.clone()));
        #[cfg(test)]
        pause_authenticated_admission_after_read();
        if let Some(entry) = generations.generations.get_mut(generation) {
            // A reattached daemon gets a fresh local tunnel, so update the endpoint
            // without disturbing jobs already pinned to this lease.
            entry.endpoint = session.clone();
        } else {
            generations.begin(generation, session.clone());
        }
        generations.activate(generation);
        write(runner_id, &generations)
    })
}

pub(crate) fn record_job_run(
    runner_id: &str,
    legacy: &RunnerSession,
    job_id: &str,
    run_id: &str,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(legacy))? else {
            return Ok(());
        };
        generations.record_run(job_id, run_id);
        write(runner_id, &generations)
    })
}

pub(crate) fn record_job_artifacts(
    runner_id: &str,
    legacy: &RunnerSession,
    job_id: &str,
    artifact_ids: impl IntoIterator<Item = String>,
) -> Result<()> {
    let artifact_ids = artifact_ids.into_iter().collect::<Vec<_>>();
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(legacy))? else {
            return Ok(());
        };
        for artifact_id in artifact_ids {
            generations.record_artifact(job_id, artifact_id);
        }
        write(runner_id, &generations)
    })
}

/// Release a durable run's generation routing claim after terminal retention
/// removes the run record. Reconciliation remains fail-closed and only stops a
/// drained, zero-job endpoint after its final owner is gone.
pub(crate) fn retire_run_owner(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    run_id: &str,
) -> Result<()> {
    retire_result_owner(runner_id, legacy, RollingResultOwnerRetirement::Run(run_id))
}

/// Release one artifact routing claim after its owning lifecycle has consumed,
/// finalized, or pruned the artifact.
pub(crate) fn retire_artifact_owner(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    artifact_id: &str,
) -> Result<()> {
    retire_result_owner(
        runner_id,
        legacy,
        RollingResultOwnerRetirement::Artifact(artifact_id),
    )
}

fn retire_result_owner(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    retirement: RollingResultOwnerRetirement<'_>,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, legacy)? else {
            return Ok(());
        };
        generations.retire_result_owner(retirement);
        // Persist before network reconciliation; a restart can retry an unreachable
        // endpoint without restoring an owner whose lifecycle was already removed.
        write(runner_id, &generations)
    })?;
    reconcile(runner_id, legacy)
}

/// Reconcile a bounded page of authoritative retained result ids. This repairs
/// stale registry claims after controller restart without guessing retention.
pub(crate) fn reconcile_result_owners(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    retained_run_ids: &std::collections::BTreeSet<String>,
    retained_artifact_ids: &std::collections::BTreeSet<String>,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, legacy)? else {
            return Ok(());
        };
        generations.reconcile_result_owners(retained_run_ids, retained_artifact_ids);
        write(runner_id, &generations)
    })?;
    reconcile(runner_id, legacy)
}

pub(crate) fn activate(
    runner_id: &str,
    current: &RunnerSession,
    generation: String,
    candidate: RunnerSession,
    draining_job_ids: &[String],
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let mut generations = read_locked(runner_id, Some(current))?.unwrap_or_else(|| {
            RollingGenerations::new(legacy_generation(current), current.clone())
        });
        let current_lease = current.remote_daemon_lease_id.as_deref();
        let draining_owner = generations
            .generations
            .iter()
            .find_map(|(generation, entry)| {
                (current_lease.is_some()
                    && entry.endpoint.remote_daemon_lease_id.as_deref() == current_lease)
                    .then(|| generation.clone())
            })
            .unwrap_or_else(|| legacy_generation(current));
        let legacy_owner = legacy_generation(current);
        if draining_owner != legacy_owner {
            for owners in [
                &mut generations.job_owners,
                &mut generations.run_owners,
                &mut generations.artifact_owners,
            ] {
                for owner in owners.values_mut() {
                    if owner == &legacy_owner {
                        *owner = draining_owner.clone();
                    }
                }
            }
        }
        // Legacy sessions have no ledger yet. Pin authoritative active work before
        // activation because `activate` retires zero-job drains immediately.
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
        generations.begin(generation.clone(), candidate);
        generations.activate(&generation);
        write(runner_id, &generations)
    })
}

/// Remove an unactivated candidate from the durable ledger. This is safe to
/// call after any candidate validation failure and never changes admission.
pub(crate) fn rollback_candidate(
    runner_id: &str,
    legacy: &RunnerSession,
    generation: &str,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(legacy))? else {
            return Ok(());
        };
        if generations.rollback(generation) {
            write(runner_id, &generations)?;
        }
        Ok(())
    })
}

/// Undo a candidate that was activated locally but could not be durably
/// published as the controller session. The previous admission remains intact.
pub(crate) fn rollback_activation(
    runner_id: &str,
    current: &RunnerSession,
    generation: &str,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(current))? else {
            return Ok(());
        };
        let previous = legacy_generation(current);
        if generations.admission_owner == generation {
            generations.generations.remove(generation);
            if let Some(entry) = generations.generations.get_mut(&previous) {
                entry.drain_state = crate::RollingDrainState::Admitting;
                generations.admission_owner = previous;
            }
        } else {
            generations.rollback(generation);
        }
        write(runner_id, &generations)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use homeboy_core::test_support;
    use serde_json::json;

    use super::*;
    use crate::{
        RunnerActiveJobState, RunnerSessionRole, RunnerSessionState, RunnerStatusReport,
        RunnerTunnelMode,
    };

    fn session(lease: &str, endpoint: &str, tunnel_pid: Option<u32>) -> RunnerSession {
        RunnerSession {
            runner_id: "runner-a".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("server-a".to_string()),
            controller_id: Some("controller-a".to_string()),
            broker_url: None,
            remote_daemon_address: Some(format!("{endpoint}:4000")),
            local_port: Some(4000),
            local_url: Some(format!("http://{endpoint}:4000")),
            tunnel_pid,
            remote_daemon_pid: Some(42),
            remote_daemon_lease_id: Some(lease.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(format!("homeboy test+{lease}")),
            connected_at: "2026-07-20T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[derive(Default)]
    struct FakeEndpointOperations {
        active_jobs: RefCell<std::collections::BTreeMap<String, usize>>,
        terminal_reconcile_failures: RefCell<std::collections::BTreeSet<String>>,
        terminal_reconciled_leases: RefCell<Vec<String>>,
        stopped_leases: RefCell<Vec<String>>,
        terminated_pids: RefCell<Vec<u32>>,
    }

    impl GenerationEndpointOperations for FakeEndpointOperations {
        fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
            let lease_id = session.remote_daemon_lease_id.clone().expect("lease");
            self.terminal_reconciled_leases
                .borrow_mut()
                .push(lease_id.clone());
            !self
                .terminal_reconcile_failures
                .borrow()
                .contains(&lease_id)
        }

        fn active_jobs(&self, session: &RunnerSession) -> Option<usize> {
            self.active_jobs
                .borrow()
                .get(session.remote_daemon_lease_id.as_deref()?)
                .copied()
        }

        fn stop(&self, session: &RunnerSession) -> bool {
            self.stopped_leases
                .borrow_mut()
                .push(session.remote_daemon_lease_id.clone().expect("lease"));
            true
        }

        fn terminate_tunnel(&self, pid: u32) {
            self.terminated_pids.borrow_mut().push(pid);
        }
    }

    fn persisted_registry(runner_id: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(path(runner_id).expect("registry path"))
            .expect("read registry");
        serde_json::from_str(&raw).expect("parse registry")
    }

    const PROCESS_SYNC_DIR_ENV: &str = "HOMEBOY_GENERATION_STORE_PROCESS_SYNC_DIR";

    fn process_sync_dir() -> std::path::PathBuf {
        std::env::var_os(PROCESS_SYNC_DIR_ENV)
            .map(std::path::PathBuf::from)
            .expect("process-isolated generation-store sync directory")
    }

    fn wait_for_process_file(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process fixture signal `{}`",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn generation_store_process(
        context: &homeboy_core::test_support::HermeticTestContext,
        test: &str,
        sync_dir: &std::path::Path,
    ) -> std::process::Command {
        let mut command = context.command(homeboy_core::test_support::TestBinary::CurrentTest);
        command
            .args(["--ignored", "--exact", test])
            .env(PROCESS_SYNC_DIR_ENV, sync_dir);
        command
    }

    #[test]
    fn process_isolated_reconciliation_preserves_concurrent_job_admission() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let sync_dir = context.root().join("generation-store-sync");
        std::fs::create_dir_all(&sync_dir).expect("create process sync directory");

        assert!(generation_store_process(
            &context,
            "generation_store::tests::process_seed_draining_generation",
            &sync_dir,
        )
        .status()
        .expect("seed process")
        .success());

        let mut reconciliation = generation_store_process(
            &context,
            "generation_store::tests::process_reconcile_draining_generation",
            &sync_dir,
        )
        .spawn()
        .expect("start reconciliation process");
        wait_for_process_file(&sync_dir.join("remote-check-complete"));

        assert!(generation_store_process(
            &context,
            "generation_store::tests::process_record_fresh_job",
            &sync_dir,
        )
        .status()
        .expect("record process")
        .success());
        std::fs::write(sync_dir.join("allow-commit"), "proceed").expect("release reconciliation");
        assert!(reconciliation
            .wait()
            .expect("reconciliation process")
            .success());

        let registry = std::fs::read_to_string(
            context
                .config_dir()
                .join("runner-sessions/runner-a/generations.json"),
        )
        .expect("read process-isolated registry");
        let registry: serde_json::Value = serde_json::from_str(&registry).expect("parse registry");
        assert_eq!(
            registry["job_owners"]["accepted-during-reconcile"], "lease-fresh",
            "the locked reconciliation commit reloads after remote work"
        );
        assert_eq!(registry["generations"]["lease-fresh"]["active_jobs"], 1);
        assert_eq!(
            registry["generations"]["lease-fresh"]["observed_active_jobs"],
            serde_json::Value::Null,
            "a concurrent admission invalidates the earlier health observation"
        );

        let mut registry: GenerationRegistry<RunnerSession> =
            serde_json::from_value(registry).expect("deserialize reconciled registry");
        registry.generations.begin(
            "lease-next",
            session("lease-next", "daemon-next", Some(303)),
        );
        registry.generations.activate("lease-next");
        assert!(registry.generations.generations.contains_key("lease-fresh"));
        assert_eq!(
            registry.generations.job_owner("accepted-during-reconcile"),
            Some("lease-fresh")
        );
    }

    #[test]
    fn process_isolated_legacy_migration_preserves_concurrent_job_admission() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let sync_dir = context.root().join("legacy-migration-sync");
        std::fs::create_dir_all(&sync_dir).expect("create process sync directory");

        assert!(generation_store_process(
            &context,
            "generation_store::tests::process_seed_legacy_generation",
            &sync_dir,
        )
        .status()
        .expect("seed legacy process")
        .success());

        let mut migration = generation_store_process(
            &context,
            "generation_store::tests::process_migrate_legacy_generation",
            &sync_dir,
        )
        .env(LEGACY_MIGRATION_SYNC_DIR_ENV, &sync_dir)
        .spawn()
        .expect("start migration process");
        wait_for_process_file(&sync_dir.join("migration-ready"));

        assert!(generation_store_process(
            &context,
            "generation_store::tests::process_record_fresh_job",
            &sync_dir,
        )
        .status()
        .expect("record process")
        .success());
        std::fs::write(sync_dir.join("allow-migration"), "proceed").expect("release migration");
        assert!(migration.wait().expect("migration process").success());

        let registry = std::fs::read_to_string(
            context
                .config_dir()
                .join("runner-sessions/runner-a/generations.json"),
        )
        .expect("read migrated registry");
        let registry: serde_json::Value = serde_json::from_str(&registry).expect("parse registry");
        assert_eq!(registry["runner_id"], "runner-a");
        assert_eq!(
            registry["job_owners"]["accepted-during-reconcile"], "lease-fresh",
            "a stale legacy migration must reload rather than erase accepted ownership"
        );
    }

    #[test]
    fn process_isolated_authenticated_admission_preserves_concurrent_job_admission() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let sync_dir = context.root().join("authenticated-admission-sync");
        std::fs::create_dir_all(&sync_dir).expect("create process sync directory");

        assert!(generation_store_process(
            &context,
            "generation_store::tests::process_seed_draining_generation",
            &sync_dir,
        )
        .status()
        .expect("seed process")
        .success());

        let mut admission = generation_store_process(
            &context,
            "generation_store::tests::process_record_authenticated_admission",
            &sync_dir,
        )
        .env(AUTHENTICATED_ADMISSION_SYNC_DIR_ENV, &sync_dir)
        .spawn()
        .expect("start authenticated admission");
        wait_for_process_file(&sync_dir.join("admission-read"));

        let mut record = generation_store_process(
            &context,
            "generation_store::tests::process_record_fresh_job",
            &sync_dir,
        )
        .spawn()
        .expect("start concurrent job record");
        std::fs::write(sync_dir.join("allow-admission"), "proceed").expect("release admission");
        assert!(admission
            .wait()
            .expect("authenticated admission process")
            .success());
        assert!(record.wait().expect("record process").success());

        let registry = std::fs::read_to_string(
            context
                .config_dir()
                .join("runner-sessions/runner-a/generations.json"),
        )
        .expect("read process-isolated registry");
        let registry: serde_json::Value = serde_json::from_str(&registry).expect("parse registry");
        assert_eq!(
            registry["job_owners"]["accepted-during-reconcile"], "lease-fresh",
            "authenticated admission must reload and preserve accepted ownership under the lock"
        );
    }

    #[test]
    #[ignore = "invoked by process_isolated_reconciliation_preserves_concurrent_job_admission"]
    fn process_seed_draining_generation() {
        let stale = session("lease-stale", "daemon-stale", Some(101));
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        let mut generations = RollingGenerations::new("lease-fresh", fresh);
        generations.begin("lease-stale", stale);
        write("runner-a", &generations).expect("seed draining generation");
    }

    #[test]
    #[ignore = "invoked by process_isolated_legacy_migration_preserves_concurrent_job_admission"]
    fn process_seed_legacy_generation() {
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        homeboy_core::engine::local_files::write_json_file(
            &path("runner-a").expect("registry path"),
            &RollingGenerations::new("lease-fresh", fresh),
        )
        .expect("seed legacy registry without runner identity");
    }

    #[test]
    #[ignore = "invoked by process_isolated_legacy_migration_preserves_concurrent_job_admission"]
    fn process_migrate_legacy_generation() {
        read("runner-a", None).expect("migrate legacy registry");
    }

    #[test]
    #[ignore = "invoked by process_isolated_reconciliation_preserves_concurrent_job_admission"]
    fn process_record_fresh_job() {
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        record_job("runner-a", &fresh, "accepted-during-reconcile")
            .expect("record job accepted during reconciliation");
    }

    #[test]
    #[ignore = "invoked by process_isolated_authenticated_admission_preserves_concurrent_job_admission"]
    fn process_record_authenticated_admission() {
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        record_authenticated_admission("runner-a", &fresh).expect("record authenticated admission");
    }

    #[test]
    #[ignore = "invoked by process_isolated_reconciliation_preserves_concurrent_job_admission"]
    fn process_reconcile_draining_generation() {
        struct ProcessBlockingOperations(std::path::PathBuf);

        impl GenerationEndpointOperations for ProcessBlockingOperations {
            fn reconcile_terminal_jobs(&self, _: &RunnerSession) -> bool {
                true
            }

            fn active_jobs(&self, _: &RunnerSession) -> Option<usize> {
                std::fs::write(self.0.join("remote-check-complete"), "checked")
                    .expect("signal remote drain check");
                wait_for_process_file(&self.0.join("allow-commit"));
                Some(0)
            }

            fn stop(&self, _: &RunnerSession) -> bool {
                true
            }

            fn terminate_tunnel(&self, _: u32) {}
        }

        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        reconcile_with(
            "runner-a",
            Some(&fresh),
            &ProcessBlockingOperations(process_sync_dir()),
        )
        .expect("reconcile after remote drain check");
    }

    #[test]
    fn missing_runner_identity_recovers_without_changing_generation_routing() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let mut prior = RollingGenerations::new("lease-a", a.clone());
            prior.admit_job("job-a");
            assert!(prior.record_run("job-a", "run-a"));
            assert!(prior.record_artifact("job-a", "artifact-a"));
            prior.begin("build-b", b.clone());
            prior.activate("build-b");
            prior.admit_job("job-b");
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &prior,
            )
            .expect("write prior registry");

            let restored = read("runner-a", None)
                .expect("load prior registry")
                .expect("registry");
            assert_eq!(restored.admission_owner, "build-b");
            assert_eq!(restored.job_owner("job-a"), Some("lease-a"));
            assert_eq!(restored.job_owner("job-b"), Some("build-b"));
            assert_eq!(
                restored.endpoint_owner(None, Some("run-a"), None),
                Some("lease-a")
            );
            assert_eq!(
                restored.endpoint_owner(None, None, Some("artifact-a")),
                Some("lease-a")
            );
            assert_eq!(
                admission_session("runner-a", None).expect("route admission"),
                Some(b)
            );
            assert_eq!(
                job_session("runner-a", "job-a", None).expect("route draining job"),
                Some(a)
            );

            let persisted = persisted_registry("runner-a");
            assert_eq!(persisted["runner_id"], "runner-a");
            assert_eq!(persisted["job_owners"]["job-a"], "lease-a");
            assert_eq!(persisted["run_owners"]["run-a"], "lease-a");
            assert_eq!(persisted["artifact_owners"]["artifact-a"], "lease-a");
        });
    }

    #[test]
    fn completed_result_ownership_requires_generation_preserving_refresh_until_retired() {
        test_support::with_isolated_home(|_| {
            let current = session("lease-a", "daemon-a", Some(101));
            let mut generations = RollingGenerations::new("lease-a", current.clone());
            generations.admit_job("job-a");
            assert!(generations.record_run("job-a", "run-a"));
            assert!(generations.record_artifact("job-a", "artifact-a"));
            assert!(!generations.complete_job("job-a"));
            write("runner-a", &generations).expect("persist retained result ownership");

            assert!(
                requires_generation_preserving_refresh("runner-a", Some(&current))
                    .expect("inspect retained ownership"),
                "completed result owners keep their producing generation routable"
            );

            generations.retire_result_owner(RollingResultOwnerRetirement::Run("run-a"));
            generations.retire_result_owner(RollingResultOwnerRetirement::Artifact("artifact-a"));
            write("runner-a", &generations).expect("persist retired result ownership");

            assert!(
                !requires_generation_preserving_refresh("runner-a", Some(&current))
                    .expect("inspect idle generation"),
                "an idle generation can use ordinary disconnect and reconnect"
            );
        });
    }

    #[test]
    fn registry_rejects_an_explicit_mismatched_runner_id() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &GenerationRegistry {
                    runner_id: "runner-b".to_string(),
                    generations: RollingGenerations::new("lease-a", a),
                },
            )
            .expect("write registry");

            let error = read("runner-a", None).expect_err("mismatched registry rejects");
            assert_eq!(error.code.as_str(), "config.invalid_value");
            assert_eq!(error.details["value"], "runner-b");
            assert!(error.details["problem"]
                .as_str()
                .expect("problem")
                .contains("runner-scoped path `runner-a`"));
        });
    }

    #[test]
    fn missing_runner_identity_rejects_empty_and_conflicting_endpoints() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let mut b = session("lease-b", "daemon-b", Some(202));
            b.runner_id = "runner-b".to_string();
            let mut generations = RollingGenerations::new("lease-a", a);
            generations.begin("build-b", b);
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &generations,
            )
            .expect("write conflicted prior registry");

            let error = read("runner-a", None).expect_err("conflicting endpoint rejects");
            assert_eq!(error.code.as_str(), "config.invalid_value");
            assert_eq!(error.details["value"], "runner-b");
            assert!(error.details["problem"]
                .as_str()
                .expect("problem")
                .contains("generation `build-b` endpoint runner ID does not match"));
            assert!(persisted_registry("runner-a").get("runner_id").is_none());

            let mut empty = session("lease-a", "daemon-a", Some(101));
            empty.runner_id.clear();
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &RollingGenerations::new("lease-a", empty),
            )
            .expect("write empty prior registry");
            let error = read("runner-a", None).expect_err("empty endpoint rejects");
            assert_eq!(error.code.as_str(), "config.invalid_value");
            assert_eq!(error.details["value"], "");
            assert!(error.details["problem"]
                .as_str()
                .expect("problem")
                .contains("empty endpoint runner ID"));
            assert!(persisted_registry("runner-a").get("runner_id").is_none());
        });
    }

    #[test]
    fn malformed_prior_registry_is_not_migrated() {
        test_support::with_isolated_home(|_| {
            let registry_path = path("runner-a").expect("registry path");
            std::fs::create_dir_all(registry_path.parent().expect("registry directory"))
                .expect("create registry directory");
            std::fs::write(&registry_path, r#"{"generations": []}"#)
                .expect("write malformed registry");

            let error = read("runner-a", None).expect_err("malformed registry rejects");
            assert_eq!(error.code.as_str(), "config.invalid_json");
            assert_eq!(
                std::fs::read_to_string(registry_path).expect("read malformed registry"),
                r#"{"generations": []}"#
            );
        });
    }

    #[test]
    fn unsupported_registry_shape_fails_closed() {
        test_support::with_isolated_home(|_| {
            let registry_path = path("runner-a").expect("registry path");
            std::fs::create_dir_all(registry_path.parent().expect("registry directory"))
                .expect("create registry directory");
            std::fs::write(
                &registry_path,
                r#"{"runner_id":"runner-a","unexpected":true}"#,
            )
            .expect("write unsupported registry");

            let error = read("runner-a", None).expect_err("unsupported registry rejects");
            assert_eq!(error.code.as_str(), "config.invalid_value");
            assert_eq!(error.details["key"], "generation_registry.unexpected");
            assert_eq!(
                std::fs::read_to_string(registry_path).expect("read unsupported registry"),
                r#"{"runner_id":"runner-a","unexpected":true}"#
            );
        });
    }

    #[test]
    fn fresh_direct_admission_replaces_a_stale_endpoint_and_preserves_draining_jobs() {
        test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "127.0.0.1:63114", Some(101));
            let fresh = session("lease-fresh", "127.0.0.1:50575", Some(202));
            record_job("runner-a", &stale, "active-stale-job").expect("record stale job");

            reconcile_admission_session("runner-a", &fresh).expect("promote fresh admission");
            record_job("runner-a", &fresh, "fresh-cook-job").expect("record one fresh dispatch");

            assert_eq!(
                admission_session("runner-a", Some(&fresh)).expect("resolve admission"),
                Some(fresh.clone()),
                "new Cook work uses the same endpoint runner status just verified"
            );
            assert_eq!(
                job_session("runner-a", "active-stale-job", Some(&fresh))
                    .expect("resolve draining job"),
                Some(stale),
                "existing work remains pinned to its draining generation"
            );
            let projection = status_projection("runner-a", Some(&fresh)).expect("projection");
            assert!(projection.iter().any(|entry| {
                entry.admission_owner
                    && entry.remote_daemon_lease_id.as_deref() == Some("lease-fresh")
                    && entry.local_url.as_deref() == Some("http://127.0.0.1:50575:4000")
                    && entry.active_job_count == 1
            }));
            assert!(projection.iter().any(|entry| {
                !entry.admission_owner
                    && entry.remote_daemon_lease_id.as_deref() == Some("lease-stale")
                    && entry.active_job_count == 1
            }));
        });
    }

    #[test]
    fn draining_generation_keeps_result_routing_after_zero_job_reconciliation() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            record_job("runner-a", &a, "job-a").expect("record A job");
            record_job_run("runner-a", &a, "job-a", "run-a").expect("record A run");
            record_job_artifacts("runner-a", &a, "job-a", ["artifact-a".to_string()])
                .expect("record A artifact");
            let initial = persisted_registry("runner-a");
            assert_eq!(initial["runner_id"], "runner-a");
            assert_eq!(initial["job_owners"]["job-a"], "lease-a");
            assert_eq!(initial["run_owners"]["run-a"], "lease-a");
            assert_eq!(initial["artifact_owners"]["artifact-a"], "lease-a");

            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("promote B");
            let promoted = persisted_registry("runner-a");
            assert_eq!(promoted["runner_id"], "runner-a");
            assert_eq!(promoted["admission_owner"], "build-b");
            assert_eq!(promoted["job_owners"]["job-a"], "lease-a");
            assert_eq!(promoted["run_owners"]["run-a"], "lease-a");
            assert_eq!(promoted["artifact_owners"]["artifact-a"], "lease-a");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 0);
            reconcile_with("runner-a", Some(&b), &operations).expect("drain A");
            let drained = persisted_registry("runner-a");
            assert_eq!(drained["runner_id"], "runner-a");
            assert_eq!(drained["admission_owner"], "build-b");
            assert!(drained["job_owners"].get("job-a").is_none());
            assert_eq!(drained["run_owners"]["run-a"], "lease-a");
            assert_eq!(drained["artifact_owners"]["artifact-a"], "lease-a");
            assert_eq!(
                endpoint_session("runner-a", None, Some("run-a"), None, Some(&b))
                    .expect("route retained run"),
                Some(a.clone())
            );
            assert_eq!(
                endpoint_session("runner-a", None, None, Some("artifact-a"), Some(&b))
                    .expect("route retained artifact"),
                Some(a)
            );
            assert!(operations.stopped_leases.borrow().is_empty());
            assert!(operations.terminated_pids.borrow().is_empty());
        });
    }

    #[test]
    fn reconciliation_replaces_stale_draining_count_with_authoritative_live_count() {
        test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            let mut generations = RollingGenerations::new("lease-fresh", fresh.clone());
            generations.begin("lease-stale", stale);
            generations
                .generations
                .get_mut("lease-fresh")
                .expect("admitting generation")
                .active_jobs = 9;
            generations
                .generations
                .get_mut("lease-stale")
                .expect("draining generation")
                .active_jobs = 18;
            write("runner-a", &generations).expect("write stale generation count");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-stale".to_string(), 2);
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-fresh".to_string(), 1);
            reconcile_with("runner-a", Some(&fresh), &operations).expect("reconcile live count");

            let projection = status_projection("runner-a", Some(&fresh)).expect("projection");
            let fresh = projection
                .iter()
                .find(|entry| entry.generation == "lease-fresh")
                .expect("admitting generation status");
            assert_eq!(fresh.active_job_count, 1);
            assert_eq!(fresh.observed_active_job_count, Some(1));
            assert!(fresh.active_job_count_authoritative);
            let stale = projection
                .iter()
                .find(|entry| entry.generation == "lease-stale")
                .expect("draining generation status");
            assert_eq!(stale.active_job_count, 2);
            assert_eq!(stale.observed_active_job_count, Some(2));
            assert!(stale.active_job_count_authoritative);
        });
    }

    #[test]
    fn unreachable_draining_generation_keeps_unverified_persisted_claims() {
        test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            let mut generations = RollingGenerations::new("lease-fresh", fresh.clone());
            generations.begin("lease-stale", stale);
            generations
                .generations
                .get_mut("lease-stale")
                .expect("draining generation")
                .active_jobs = 18;
            generations
                .job_owners
                .insert("job-stale".to_string(), "lease-stale".to_string());
            write("runner-a", &generations).expect("write unreachable generation");

            let operations = FakeEndpointOperations::default();
            reconcile_with("runner-a", Some(&fresh), &operations)
                .expect("reconcile unreachable endpoint");

            let projection = status_projection("runner-a", Some(&fresh)).expect("projection");
            let stale = projection
                .iter()
                .find(|entry| entry.generation == "lease-stale")
                .expect("draining generation status");
            assert_eq!(stale.active_job_count, 18);
            assert_eq!(stale.observed_active_job_count, None);
            assert!(!stale.active_job_count_authoritative);
            assert_eq!(stale.job_owner_count, 1);
            assert_eq!(
                job_session("runner-a", "job-stale", Some(&fresh)).expect("route persisted job"),
                Some(session("lease-stale", "daemon-stale", Some(101)))
            );
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    #[test]
    fn persisted_generation_rotation_routes_jobs_projects_status_and_retires_once() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            // No generations file is the deployed legacy shape. Its active
            // session becomes generation A without losing its job ownership.
            assert_eq!(
                admission_session("runner-a", Some(&a)).expect("admission"),
                Some(a.clone())
            );
            record_job("runner-a", &a, "job-a").expect("record A job");
            assert_eq!(
                job_session("runner-a", "job-a", Some(&a)).expect("route A job"),
                Some(a.clone())
            );

            // Candidate B has already passed startup/tunnel validation. Only
            // this activation moves admission; a failed candidate leaves A intact.
            let before_candidate_activation = read("runner-a", Some(&a))
                .expect("read A")
                .expect("A ledger");
            assert_eq!(before_candidate_activation.admission_owner, "lease-a");
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("activate validated B");
            record_job("runner-a", &b, "job-b").expect("record B job");

            // Reloading the persisted ledger simulates a controller restart.
            let reloaded = read("runner-a", Some(&a)).expect("reload").expect("ledger");
            assert_eq!(reloaded.admission_owner, "build-b");
            assert_eq!(reloaded.job_owner("job-a"), Some("lease-a"));
            assert_eq!(reloaded.job_owner("job-b"), Some("build-b"));
            assert_eq!(
                admission_session("runner-a", Some(&a)).expect("route admission"),
                Some(b.clone())
            );
            assert_eq!(
                job_session("runner-a", "job-a", Some(&a)).expect("route A operation"),
                Some(a.clone())
            );
            assert_eq!(
                job_session("runner-a", "job-b", Some(&a)).expect("route B operation"),
                Some(b.clone())
            );

            let projected = status_projection("runner-a", Some(&a)).expect("status projection");
            assert_eq!(projected.len(), 2);
            assert!(projected.iter().any(|entry| entry.generation == "lease-a"
                && entry.active_job_count == 1
                && !entry.admission_owner));
            assert!(projected.iter().any(|entry| entry.generation == "build-b"
                && entry.active_job_count == 1
                && entry.admission_owner));
            let report = RunnerStatusReport {
                runner_id: "runner-a".to_string(),
                connected: true,
                state: RunnerSessionState::Connected,
                session: Some(b.clone()),
                stale_daemon: None,
                daemon_freshness: None,
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                stale_runner_jobs: Vec::new(),
                active_job_count: 1,
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::Available,
                active_job_source: None,
                active_job_error: None,
                active_job_recovery_evidence: None,
                session_path: "test".to_string(),
            };
            assert_eq!(
                serde_json::to_value(report).expect("serialize status")["generations"],
                json!(projected)
            );

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 1);
            reconcile_with("runner-a", Some(&b), &operations).expect("busy A remains");
            assert_eq!(
                status_projection("runner-a", Some(&b))
                    .expect("projection")
                    .len(),
                2
            );

            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 0);
            reconcile_with("runner-a", Some(&b), &operations).expect("retire A");
            reconcile_with("runner-a", Some(&b), &operations).expect("idempotent reconcile");
            assert_eq!(operations.stopped_leases.into_inner(), vec!["lease-a"]);
            assert_eq!(operations.terminated_pids.into_inner(), vec![101]);
            let final_projection =
                status_projection("runner-a", Some(&b)).expect("final projection");
            assert_eq!(final_projection.len(), 1);
            assert_eq!(final_projection[0].generation, "build-b");
            assert_eq!(final_projection[0].active_job_count, 1);
        });
    }

    #[test]
    fn reconciliation_rebuilds_draining_counts_and_retires_terminal_generations() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let c = session("lease-c", "daemon-c", Some(303));

            record_job("runner-a", &a, "job-a").expect("record A job");
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("promote B");
            record_job("runner-a", &b, "job-b").expect("record B job");
            activate(
                "runner-a",
                &b,
                "build-c".to_string(),
                c.clone(),
                &["job-b".to_string()],
            )
            .expect("promote C");
            record_job("runner-a", &c, "job-c").expect("record C job");

            // Simulate stale persisted ownership from an earlier controller.
            let mut persisted = read("runner-a", Some(&c))
                .expect("read registry")
                .expect("registry");
            persisted
                .generations
                .get_mut("lease-a")
                .expect("A")
                .active_jobs = 9;
            persisted
                .generations
                .get_mut("build-b")
                .expect("B")
                .active_jobs = 18;
            write("runner-a", &persisted).expect("write stale counts");

            // Reload first to prove reconciliation does not depend on controller memory.
            let restored = read("runner-a", Some(&c))
                .expect("restart read")
                .expect("registry");
            assert_eq!(restored.generations["lease-a"].active_jobs, 9);
            assert_eq!(restored.generations["build-b"].active_jobs, 18);

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .extend([("lease-a".to_string(), 0), ("lease-b".to_string(), 1)]);
            reconcile_with("runner-a", Some(&c), &operations).expect("reconcile terminal A");

            let projected = status_projection("runner-a", Some(&c)).expect("project after A");
            assert_eq!(projected.len(), 2);
            assert!(projected
                .iter()
                .any(|entry| entry.generation == "build-b" && entry.active_job_count == 1));
            assert!(projected
                .iter()
                .any(|entry| entry.generation == "build-c" && entry.active_job_count == 1));
            assert_eq!(operations.stopped_leases.borrow().as_slice(), ["lease-a"]);
            assert_eq!(operations.terminated_pids.borrow().as_slice(), [101]);
            assert_eq!(
                operations.terminal_reconciled_leases.borrow().as_slice(),
                ["lease-b", "lease-a"]
            );

            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-b".to_string(), 0);
            reconcile_with("runner-a", Some(&c), &operations).expect("reconcile terminal B");
            let projected = status_projection("runner-a", Some(&c)).expect("final projection");
            assert_eq!(projected.len(), 1);
            assert_eq!(projected[0].generation, "build-c");
            assert_eq!(projected[0].active_job_count, 1);
            assert_eq!(
                operations.stopped_leases.borrow().as_slice(),
                ["lease-a", "lease-b"]
            );
            assert_eq!(operations.terminated_pids.borrow().as_slice(), [101, 202]);
        });
    }

    #[test]
    fn failed_terminal_reconciliation_keeps_the_generation_draining() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            record_job("runner-a", &a, "job-a").expect("record A job");
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("promote B");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 0);
            operations
                .terminal_reconcile_failures
                .borrow_mut()
                .insert("lease-a".to_string());

            reconcile_with("runner-a", Some(&b), &operations)
                .expect("failed settlement remains fail-closed");

            let projected = status_projection("runner-a", Some(&b)).expect("projection");
            assert!(projected
                .iter()
                .any(|entry| entry.generation == "lease-a" && entry.active_job_count == 1));
            assert!(operations.stopped_leases.borrow().is_empty());
            assert!(operations.terminated_pids.borrow().is_empty());
        });
    }

    #[test]
    fn second_rotation_keeps_jobs_owned_by_the_named_draining_generation() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let c = session("lease-c", "daemon-c", Some(303));

            activate("runner-a", &a, "build-b".to_string(), b.clone(), &[]).expect("promote B");
            record_job("runner-a", &b, "job-b").expect("record B job");

            activate(
                "runner-a",
                &b,
                "build-c".to_string(),
                c.clone(),
                &["job-b".to_string()],
            )
            .expect("promote C");

            let generations = read("runner-a", Some(&c))
                .expect("read generations")
                .expect("generation registry");
            assert_eq!(generations.job_owner("job-b"), Some("build-b"));
            assert_eq!(
                job_session("runner-a", "job-b", Some(&c)).expect("route B job"),
                Some(b)
            );
            assert_eq!(generations.generations["build-b"].active_jobs, 1);
            assert_eq!(generations.admission_owner, "build-c");
        });
    }

    #[test]
    fn persisted_lease_alias_routes_to_its_named_generation() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let mut generations = RollingGenerations::new("build-a", a.clone());
            generations
                .generations
                .get_mut("build-a")
                .expect("build A generation")
                .active_jobs = 1;
            generations
                .job_owners
                .insert("job-a".to_string(), "lease-a".to_string());
            generations
                .run_owners
                .insert("run-a".to_string(), "lease-a".to_string());
            generations.begin("build-b", b.clone());
            generations.activate("build-b");
            write("runner-a", &generations).expect("persist lease aliases");

            assert_eq!(
                job_session("runner-a", "job-a", Some(&b)).expect("route aliased job"),
                Some(a.clone())
            );
            assert_eq!(
                endpoint_session("runner-a", None, Some("run-a"), None, Some(&b))
                    .expect("route aliased run"),
                Some(a)
            );
        });
    }

    #[test]
    fn legacy_rotation_pins_active_a_jobs_before_b_can_retire_it() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            // This is the deployed pre-generation state: there is no ledger,
            // but the daemon reports authoritative active job identities.
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["legacy-job-a".to_string()],
            )
            .expect("activate B without losing legacy A work");

            let ledger = read("runner-a", Some(&b))
                .expect("read ledger")
                .expect("ledger");
            assert_eq!(ledger.generations["lease-a"].active_jobs, 1);
            assert_eq!(ledger.job_owner("legacy-job-a"), Some("lease-a"));
            assert_eq!(
                job_session("runner-a", "legacy-job-a", Some(&b)).expect("route A job"),
                Some(a)
            );
            assert_eq!(
                admission_session("runner-a", Some(&b)).expect("route admissions"),
                Some(b)
            );
        });
    }

    #[test]
    fn activation_rollback_restores_a_after_post_validation_failure() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b,
                &["job-a".to_string()],
            )
            .expect("activate B");

            // This is the same ledger state reached when either durable
            // activation publication or controller-session publication fails.
            rollback_activation("runner-a", &a, "build-b").expect("restore A");
            let restored = read("runner-a", Some(&a)).expect("read").expect("ledger");
            assert_eq!(restored.admission_owner, "lease-a");
            assert_eq!(restored.job_owner("job-a"), Some("lease-a"));
            assert!(!restored.generations.contains_key("build-b"));
        });
    }
}
