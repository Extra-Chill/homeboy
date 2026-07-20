use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::Duration;

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

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
        generations.job_owner(job_id).and_then(|owner| {
            generations
                .generations
                .get(owner)
                .map(|generation| generation.endpoint.clone())
        })
    }))
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
            .and_then(|owner| generations.generations.get(owner))
            .map(|generation| generation.endpoint.clone())
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
    fn active_jobs(&self, session: &RunnerSession) -> Option<u64>;
    fn stop(&self, session: &RunnerSession) -> bool;
    fn terminate_tunnel(&self, pid: u32);
}

struct HttpGenerationEndpointOperations {
    client: reqwest::blocking::Client,
}

impl GenerationEndpointOperations for HttpGenerationEndpointOperations {
    fn active_jobs(&self, session: &RunnerSession) -> Option<u64> {
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
/// endpoint remains recorded and routable. Only its own health response may
/// authorize the zero-job lease-bound stop.
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
    let drained = generations
        .generations
        .iter()
        .filter_map(|(generation, entry)| {
            (entry.drain_state == crate::RollingDrainState::Draining)
                .then_some((generation.clone(), entry.endpoint.clone()))
        })
        .filter_map(|(generation, session)| {
            (operations.active_jobs(&session) == Some(0)).then_some((generation, session))
        })
        .collect::<Vec<_>>();
    let retired = drained
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
        for generation in &retired {
            if generations
                .generations
                .get(generation)
                .is_some_and(|entry| {
                    entry.drain_state == crate::RollingDrainState::Draining
                        && entry.active_jobs == 0
                })
            {
                generations.generations.remove(generation);
                generations
                    .job_owners
                    .retain(|_, owner| owner != generation);
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
        generations.admit_job(job_id);
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
    let mut generations = read(runner_id, None)?
        .unwrap_or_else(|| RollingGenerations::new(generation, session.clone()));
    if let Some(entry) = generations.generations.get_mut(generation) {
        // A reattached daemon gets a fresh local tunnel, so update the endpoint
        // without disturbing jobs already pinned to this lease.
        entry.endpoint = session.clone();
    } else {
        generations.begin(generation, session.clone());
    }
    generations.activate(generation);
    write(runner_id, &generations)
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
        let draining_owner = legacy_generation(current);
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
        active_jobs: RefCell<std::collections::BTreeMap<String, u64>>,
        stopped_leases: RefCell<Vec<String>>,
        terminated_pids: RefCell<Vec<u32>>,
    }

    impl GenerationEndpointOperations for FakeEndpointOperations {
        fn active_jobs(&self, session: &RunnerSession) -> Option<u64> {
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
    #[ignore = "invoked by process_isolated_reconciliation_preserves_concurrent_job_admission"]
    fn process_reconcile_draining_generation() {
        struct ProcessBlockingOperations(std::path::PathBuf);

        impl GenerationEndpointOperations for ProcessBlockingOperations {
            fn active_jobs(&self, _: &RunnerSession) -> Option<u64> {
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
    fn atomic_rewrites_keep_runner_identity_through_promotion_and_drain() {
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
