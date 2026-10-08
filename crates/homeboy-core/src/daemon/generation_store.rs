//! Stable routing state for local daemon generations.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs4::fs_std::FileExt;

use homeboy_engine_primitives::rolling_generation::{
    RollingDrainState, RollingGenerations, RollingStart,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::DaemonState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct LocalDaemonEndpoint {
    pub lease_id: String,
    pub address: String,
    pub state_dir: String,
    pub build_identity: String,
}

impl LocalDaemonEndpoint {
    fn from_state(state: &DaemonState) -> Self {
        Self {
            lease_id: state.lease_id.clone(),
            address: state.address.clone(),
            state_dir: Path::new(&state.state_path)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .display()
                .to_string(),
            build_identity: state.build_identity.display.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LocalDaemonGenerationRegistry {
    schema: String,
    generations: RollingGenerations<LocalDaemonEndpoint>,
}

const SCHEMA: &str = "homeboy.daemon.generations.v1";
pub(super) const DAEMON_ROUTER_DIR_ENV: &str = "HOMEBOY_DAEMON_ROUTER_DIR";
pub(super) const DAEMON_ROUTER_BYPASS_ENV: &str = "HOMEBOY_DAEMON_ROUTER_BYPASS";

pub(super) fn bypassed() -> bool {
    std::env::var_os(DAEMON_ROUTER_BYPASS_ENV).is_some()
}

fn registry_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(DAEMON_ROUTER_DIR_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("generations.json"));
    }
    let state = crate::paths::daemon_state_file()?;
    Ok(state.with_file_name("generations.json"))
}

pub(super) fn router_dir() -> Result<PathBuf> {
    registry_path()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::internal_unexpected("daemon generation registry has no parent"))
}

fn read_registry() -> Result<Option<LocalDaemonGenerationRegistry>> {
    read_registry_at(&registry_path()?)
}

/// Read the generation registry stored in `router_dir`, without locking or
/// mutating it. `None` means no registry exists there yet (a daemon that never
/// handed off). Lifecycle observation uses this to inspect any router
/// directory, not only the one this process is configured for (#15557).
pub(super) fn generations_at(
    router_dir: &Path,
) -> Result<Option<RollingGenerations<LocalDaemonEndpoint>>> {
    Ok(
        read_registry_at(&router_dir.join("generations.json"))?
            .map(|registry| registry.generations),
    )
}

fn read_registry_at(path: &Path) -> Result<Option<LocalDaemonGenerationRegistry>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            Error::internal_json(error.to_string(), Some(format!("parse {}", path.display())))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!("read {}", path.display())),
        )),
    }
}

fn mutate_registry<T>(
    mutation: impl FnOnce(&mut Option<LocalDaemonGenerationRegistry>) -> Result<T>,
) -> Result<T> {
    static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _process_lock = PROCESS_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("daemon generation registry mutex poisoned");
    let path = registry_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal_unexpected("daemon generation registry has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let lock_path = parent.join(".generations.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("open {}", lock_path.display())),
            )
        })?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("lock {}", lock_path.display())),
        )
    })?;
    let mut registry = read_registry()?;
    let output = mutation(&mut registry)?;
    if let Some(registry) = registry.as_mut() {
        // Older pinned binaries read `active_jobs` from disk to decide
        // retirement, so every registry write refreshes the write-only
        // compatibility counter from the owner ledger, whose terminal-job
        // routes keep it at or above the live count.
        registry.generations.sync_compat_active_jobs();
        write_registry(registry)?;
    }
    Ok(output)
}

fn write_registry(registry: &LocalDaemonGenerationRegistry) -> Result<()> {
    let path = registry_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal_unexpected("daemon generation registry has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(registry).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize daemon generations".to_string()),
        )
    })?;
    let mut temporary_file = File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    use std::io::Write;
    temporary_file.write_all(&bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    temporary_file.sync_all().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("sync {}", temporary.display())),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", path.display())),
        )
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })
}

pub(super) fn seed(state: &DaemonState) -> Result<()> {
    mutate_registry(|registry| {
        if registry.is_none() {
            let endpoint = LocalDaemonEndpoint::from_state(state);
            *registry = Some(LocalDaemonGenerationRegistry {
                schema: SCHEMA.to_string(),
                generations: RollingGenerations::new(endpoint.lease_id.clone(), endpoint),
            });
        }
        Ok(())
    })
}

/// Move admission to the serving daemon when the registered admission owner is
/// proven dead (#15456).
///
/// `seed` only creates a registry; it never touches an existing one. A daemon
/// restarted in place (a service-managed runner after a crash or upgrade) used
/// to leave `admission_owner` pointing at the dead generation, so every status
/// probe followed that dead lease and reported the live daemon as not ready.
///
/// Only a dead owner is replaced: a live owner (the blue-green case, where the
/// previous generation still serves its admitted work) keeps admission. The
/// dead generation's entry, job routing, and active-job count are preserved,
/// so its work stays attributable for recovery. Returns whether admission moved.
pub(super) fn claim_admission_from_dead_owner(
    serving: &DaemonState,
    owner_is_dead: impl Fn(&LocalDaemonEndpoint) -> bool,
) -> Result<bool> {
    mutate_registry(|registry| {
        let Some(registry) = registry.as_mut() else {
            return Ok(false);
        };
        let serving_lease = serving.lease_id.as_str();
        let owner = registry.generations.admission_owner.clone();
        if owner == serving_lease {
            return Ok(false);
        }
        let owner_dead = registry
            .generations
            .generations
            .get(&owner)
            .is_none_or(|entry| owner_is_dead(&entry.endpoint));
        if !owner_dead {
            return Ok(false);
        }
        registry.generations.begin(
            serving_lease.to_string(),
            LocalDaemonEndpoint::from_state(serving),
        );
        Ok(registry
            .generations
            .activate_preserving_drained(serving_lease))
    })
}

pub(super) fn admitting() -> Result<Option<LocalDaemonEndpoint>> {
    Ok(read_registry()?.and_then(|registry| {
        registry
            .generations
            .generations
            .get(&registry.generations.admission_owner)
            .map(|entry| entry.endpoint.clone())
    }))
}

/// Locate a registered generation by its lease so lifecycle recovery can act on
/// the same state directory that status used to authorize the operation.
pub(super) fn endpoint_for_lease(lease_id: &str) -> Result<Option<LocalDaemonEndpoint>> {
    Ok(read_registry()?.and_then(|registry| {
        registry
            .generations
            .generations
            .get(lease_id)
            .map(|entry| entry.endpoint.clone())
    }))
}

pub(super) fn endpoint_for_job(job_id: &str) -> Result<Option<LocalDaemonEndpoint>> {
    endpoint_for_job_in_router_dir(job_id, &router_dir()?)
}

/// Resolve a job owner from a specific installation's daemon-generation
/// registry. Lifecycle operations use this form so an injected data root never
/// consults the invoking process's ambient router registry.
pub(super) fn endpoint_for_job_in_router_dir(
    job_id: &str,
    router_dir: &Path,
) -> Result<Option<LocalDaemonEndpoint>> {
    let registry = read_registry_at(&router_dir.join("generations.json"))?;
    Ok(registry.and_then(|registry| {
        registry.generations.job_owner(job_id).and_then(|owner| {
            registry
                .generations
                .generations
                .get(owner)
                .map(|entry| entry.endpoint.clone())
        })
    }))
}

#[cfg(test)]
pub(super) fn record_job(job_id: &str, lease_id: &str) -> Result<()> {
    mutate_registry(|registry| {
        let registry = registry.as_mut().ok_or_else(|| {
            Error::internal_unexpected("local daemon admitted a job without a generation registry")
        })?;
        if !registry.generations.admit_job_for(lease_id, job_id)
            && registry.generations.job_owner(job_id).is_none()
        {
            return Err(Error::internal_unexpected(
                "local daemon job owner was not present in the generation registry",
            ));
        }
        Ok(())
    })
}

/// Record one new durable admission against a lease the requesting client
/// named, self-healing the registry invariant that previously made a
/// replacement daemon reject its own admissions (#14706).
///
/// A request may still carry the lease of a generation that an earlier
/// recovery retired before its successor started. The registry can prove
/// exactly that shape: the lease has no endpoint registered, so it owns no
/// durable routing and no active work. Binding such an unowned admission to
/// the live serving generation keeps the plan-then-admission sequence
/// executable; any lease that *is* registered keeps its exact routing.
pub(super) fn record_job_for_admission(
    job_id: &str,
    requested_lease_id: &str,
    serving: &DaemonState,
) -> Result<()> {
    mutate_registry(|registry| {
        let registry = registry.as_mut().ok_or_else(|| {
            Error::internal_unexpected("local daemon admitted a job without a generation registry")
        })?;
        if registry.generations.job_owner(job_id).is_some() {
            return Ok(());
        }
        if registry
            .generations
            .admit_job_for(requested_lease_id, job_id)
        {
            return Ok(());
        }
        let serving_lease_id = serving.lease_id.as_str();
        if !registry
            .generations
            .generations
            .contains_key(serving_lease_id)
        {
            registry.generations.begin(
                serving_lease_id.to_string(),
                LocalDaemonEndpoint::from_state(serving),
            );
        }
        // The serving daemon is the only live authority in this registry, so
        // it owns new admissions whose requested lease cannot be honored.
        registry
            .generations
            .activate_preserving_drained(serving_lease_id);
        if !registry.generations.admit_job_for(serving_lease_id, job_id) {
            return Err(Error::internal_unexpected(format!(
                "serving daemon lease `{serving_lease_id}` is not a registered generation after registry repair"
            )));
        }
        Ok(())
    })
}

/// Count a generation's active jobs from its own durable job store:
/// non-terminal jobs whose `job_owners` entry names that generation.
///
/// The stored `active_jobs` counter is write-only compatibility bookkeeping
/// that registry writes refresh from the owner ledger; only each
/// generation's own `jobs.json` owns terminality. The owner filter
/// attributes a shared store's jobs to exactly one generation, because a
/// replacement daemon can restart in the dead lease's directory and share
/// its store. A job compacted out of the store no longer counts. An
/// unreadable store is an error so callers can fail closed and never retire
/// the generation on incomplete evidence.
fn generation_active_jobs(
    registry: &RollingGenerations<LocalDaemonEndpoint>,
    lease_id: &str,
) -> Result<usize> {
    let Some(entry) = registry.generations.get(lease_id) else {
        return Ok(0);
    };
    let store = crate::api_jobs::JobStore::open_without_reconciliation(
        Path::new(&entry.endpoint.state_dir).join("jobs.json"),
    )?;
    Ok(store
        .list()
        .into_iter()
        .filter(|job| !job.status.is_terminal())
        .filter(|job| registry.job_owner(&job.id.to_string()) == Some(lease_id))
        .count())
}

/// Whether a generation owns no non-terminal durable job, derived from its
/// own `jobs.json`. An unreadable store counts as busy.
fn generation_is_idle(registry: &RollingGenerations<LocalDaemonEndpoint>, lease_id: &str) -> bool {
    generation_active_jobs(registry, lease_id)
        .map(|count| count == 0)
        .unwrap_or(false)
}

/// Retire drained generations whose derived active-job count is zero.
///
/// This is the daemon's drain retirement for the registry, replacing the
/// counter-based retirement the shared primitive used to perform: the
/// stored counter is write-only compatibility bookkeeping, so only the
/// derived count can prove a drained generation idle. Mirrors the retirement
/// protocol of `reconcile_drained_generations`: terminal custody is archived
/// before the entry and its job routes are removed, and the admission owner
/// is never retired here. An unreadable store counts as busy. Runs inside an
/// open registry mutation.
fn retire_derived_idle_drained_generations(
    registry: &mut LocalDaemonGenerationRegistry,
) -> Result<()> {
    let admission_owner = registry.generations.admission_owner.clone();
    let idle = registry
        .generations
        .generations
        .iter()
        .filter(|(lease_id, generation)| {
            generation.drain_state == RollingDrainState::Draining
                && lease_id.as_str() != admission_owner
                && generation_is_idle(&registry.generations, lease_id)
        })
        .map(|(lease_id, _)| lease_id.clone())
        .collect::<Vec<_>>();
    for lease_id in &idle {
        let state_dir = registry.generations.generations[lease_id]
            .endpoint
            .state_dir
            .clone();
        archive_generation_terminal_custody(&state_dir)?;
        registry.generations.generations.remove(lease_id);
        registry
            .generations
            .job_owners
            .retain(|_, owner| owner != lease_id);
    }
    Ok(())
}

/// Remove one exact dead+idle generation endpoint from the registry.
///
/// Retirement of a stopped lease previously left its registry generation
/// behind, so a successor's admission could be rejected with "job owner was
/// not present in the generation registry". This cleanup runs only for a
/// generation that is dead, idle (no non-terminal durable job of its own),
/// and registered against the exact state directory that proved it retired;
/// a live or busy generation, or a mismatched frame, is left untouched.
pub(super) fn retire_exact_dead_generation(lease_id: &str, state_dir: &str) -> Result<bool> {
    mutate_registry(|registry| {
        let Some(registry) = registry.as_mut() else {
            return Ok(false);
        };
        let matched = registry
            .generations
            .generations
            .get(lease_id)
            .is_some_and(|entry| entry.endpoint.state_dir == state_dir)
            && generation_is_idle(&registry.generations, lease_id);
        if !matched {
            return Ok(false);
        }
        archive_generation_terminal_custody(state_dir)?;
        registry.generations.generations.remove(lease_id);
        registry
            .generations
            .job_owners
            .retain(|_, owner| owner != lease_id);
        if registry.generations.admission_owner == lease_id
            && !registry.generations.generations.is_empty()
        {
            // A non-empty registry re-points admission at a remaining
            // generation; an empty one is re-seeded by the next daemon start.
            // The shared primitive never retires draining generations on
            // activation, and the stored counter is write-only bookkeeping,
            // so the derived sweep below owns that decision.
            let fallback = registry
                .generations
                .generations
                .keys()
                .next_back()
                .cloned()
                .expect("checked non-empty");
            registry.generations.activate_preserving_drained(&fallback);
            retire_derived_idle_drained_generations(registry)?;
        }
        Ok(true)
    })
}

/// Move a queued job away from the exact daemon lease proven dead during
/// startup recovery. Ordinary admission retries intentionally retain their
/// original owner; only the owner-locked death proof may use this transfer.
pub(super) fn transfer_proven_dead_job(
    job_id: &str,
    proven_dead_lease_id: &str,
    replacement: &DaemonState,
) -> Result<()> {
    mutate_registry(|registry| {
        let registry = registry.as_mut().ok_or_else(|| {
            Error::internal_unexpected("local daemon recovery has no generation registry")
        })?;
        if registry
            .generations
            .generations
            .get(proven_dead_lease_id)
            .is_none_or(|generation| generation.endpoint.lease_id != proven_dead_lease_id)
        {
            return Err(Error::validation_invalid_argument(
                "proven_dead_lease_id",
                "proven-dead daemon lease is not an exact generation endpoint",
                Some(proven_dead_lease_id.to_string()),
                None,
            ));
        }
        let owner = registry
            .generations
            .job_owner(job_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "recovered job has no durable daemon generation owner",
                    Some(job_id.to_string()),
                    None,
                )
            })?
            .to_string();
        let replacement_lease_id = &replacement.lease_id;
        if owner != proven_dead_lease_id && owner != *replacement_lease_id {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "recovered job is owned by a different daemon generation",
                Some(job_id.to_string()),
                None,
            ));
        }

        if !registry
            .generations
            .generations
            .contains_key(replacement_lease_id)
        {
            registry.generations.begin(
                replacement_lease_id.clone(),
                LocalDaemonEndpoint::from_state(replacement),
            );
        }
        if owner == *replacement_lease_id {
            return Ok(());
        }

        // Routing only: the replacement generation's active-job count is
        // derived from its own jobs.json, so the transfer needs no counter
        // shuffle and cannot double-count a job in a shared store.
        registry
            .generations
            .job_owners
            .insert(job_id.to_string(), replacement_lease_id.clone());
        Ok(())
    })
}

pub(super) fn activate(state: &DaemonState) -> Result<()> {
    mutate_registry(|registry| {
        let registry = registry.as_mut().ok_or_else(|| {
            Error::internal_unexpected("local daemon rotation has no seeded generation registry")
        })?;
        let endpoint = LocalDaemonEndpoint::from_state(state);
        if registry
            .generations
            .begin(endpoint.lease_id.clone(), endpoint)
            == RollingStart::Start
        {
            // `begin` leaves candidates draining. Only a caller that has checked the
            // published lease and exact build may expose it to admissions.
        }
        // A daemon generation must be lease-stopped before its routing entry is
        // retired. Generic rolling users retain the normal eager cleanup.
        registry
            .generations
            .activate_preserving_drained(&state.lease_id);
        Ok(())
    })
}

/// Rebuild one durable job-to-generation binding during startup recovery.
///
/// Unlike `record_job`, a generation that is no longer registered is ordinary
/// history here rather than an invariant violation. Startup replays leases
/// recorded by earlier daemons, so restarting must never require that every
/// generation which ever admitted a job still be present in the registry.
pub(super) fn rebuild_job_owner(job_id: &str, lease_id: &str) -> Result<()> {
    mutate_registry(|registry| {
        let Some(registry) = registry.as_mut() else {
            return Ok(());
        };
        if registry.generations.generations.contains_key(lease_id) {
            registry.generations.admit_job_for(lease_id, job_id);
        }
        Ok(())
    })
}

pub(super) fn reconcile_drained_generations(
    serving_lease_id: &str,
    stop: impl Fn(&LocalDaemonEndpoint) -> Result<()>,
) -> Result<()> {
    let Some(registry) = read_registry()? else {
        return Ok(());
    };
    let endpoints = registry
        .generations
        .generations
        .iter()
        .filter(|(lease_id, generation)| {
            lease_id.as_str() != serving_lease_id
                && generation.drain_state == RollingDrainState::Draining
                && generation_is_idle(&registry.generations, lease_id)
        })
        .map(|(lease_id, generation)| (lease_id.clone(), generation.endpoint.clone()))
        .collect::<Vec<_>>();
    let mut first_error = None;
    for (lease_id, endpoint) in endpoints {
        let result = stop(&endpoint).and_then(|()| {
            mutate_registry(|registry| {
                let Some(registry) = registry.as_mut() else {
                    return Ok(());
                };
                let can_retire =
                    registry
                        .generations
                        .generations
                        .get(&lease_id)
                        .is_some_and(|generation| {
                            generation.drain_state == RollingDrainState::Draining
                                && generation_is_idle(&registry.generations, &lease_id)
                        });
                if can_retire {
                    archive_generation_terminal_custody(&endpoint.state_dir)?;
                    registry.generations.generations.remove(&lease_id);
                    registry
                        .generations
                        .job_owners
                        .retain(|_, owner| owner != &lease_id);
                }
                Ok(())
            })
        });
        // One blocked lease must not prevent independent idle generations from
        // being retired. Failed entries remain durable for the next pass.
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn retained_jobs_path() -> Result<PathBuf> {
    Ok(router_dir()?.join("retained-controller-jobs.json"))
}

fn archive_generation_terminal_custody(state_dir: &str) -> Result<()> {
    crate::api_jobs::JobStore::open_without_reconciliation(Path::new(state_dir).join("jobs.json"))?
        .archive_terminal_controller_jobs(&retained_jobs_path()?)
}

/// Read original terminal custody even after the generation endpoint retires.
/// A missing legacy store or receipt returns unknown, not fabricated completion.
pub(super) fn terminal_controller_job(job_id: uuid::Uuid) -> Result<Option<crate::api_jobs::Job>> {
    let mut paths = vec![retained_jobs_path()?, crate::paths::daemon_jobs_file()?];
    if let Some(endpoint) = endpoint_for_job(&job_id.to_string())? {
        paths.insert(0, Path::new(&endpoint.state_dir).join("jobs.json"));
    }
    paths.dedup();
    for path in paths {
        if let Some(job) = crate::api_jobs::JobStore::terminal_controller_job_at(&path, job_id)? {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

pub(super) fn generation_state_dir() -> Result<PathBuf> {
    // A replacement inherits HOMEBOY_DAEMON_STATE_DIR from its generation. The
    // router location is stable across that handoff and therefore owns sibling
    // generation allocation.
    Ok(router_dir()?
        .join("generations")
        .join(uuid::Uuid::new_v4().to_string()))
}

pub(super) fn generations() -> Result<Vec<LocalDaemonEndpoint>> {
    Ok(read_registry()?
        .map(|registry| {
            registry
                .generations
                .generations
                .values()
                .map(|entry| entry.endpoint.clone())
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
pub(super) fn is_draining(lease_id: &str) -> Result<bool> {
    Ok(read_registry()?.is_some_and(|registry| {
        registry
            .generations
            .generations
            .get(lease_id)
            .is_some_and(|entry| entry.drain_state == RollingDrainState::Draining)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_identity;
    use crate::test_support::with_isolated_home;

    struct EnvVarGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let prior = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn state(lease_id: &str, address: &str) -> DaemonState {
        DaemonState {
            schema: super::super::DAEMON_LEASE_SCHEMA.to_string(),
            lease_id: lease_id.to_string(),
            startup_token: "test".to_string(),
            address: address.to_string(),
            pid: 1,
            state_path: crate::paths::daemon_state_file()
                .expect("state path")
                .display()
                .to_string(),
            started_at: "now".to_string(),
            last_seen_at: "now".to_string(),
            build_identity: build_identity::current(),
            binary_sha256: None,
            runtime_paths: super::super::DaemonRuntimeSnapshot {
                loaded_at: "now".to_string(),
                paths: Vec::new(),
            },
        }
    }

    fn state_dir_of(state: &DaemonState) -> &str {
        state
            .state_path
            .strip_suffix("/state.json")
            .expect("state path has filename")
    }

    /// Open one registered generation's own durable job store, stamped with
    /// its lease so created jobs carry the generation's `daemon_lease_id`.
    fn open_generation_store(lease_id: &str) -> crate::api_jobs::JobStore {
        let endpoint = endpoint_for_lease(lease_id)
            .expect("endpoint lookup")
            .expect("registered generation");
        crate::api_jobs::JobStore::open_without_reconciliation(
            Path::new(&endpoint.state_dir).join("jobs.json"),
        )
        .expect("open generation job store")
        .with_daemon_lease(lease_id.to_string())
    }

    /// Admit one durable non-terminal job owned by `lease_id` in the
    /// generation's own store, and record its registry owner.
    fn admitted_job(lease_id: &str, operation: &str) -> uuid::Uuid {
        let store = open_generation_store(lease_id);
        let job = store.create(operation);
        record_job(&job.id.to_string(), lease_id).expect("record owner");
        job.id
    }

    #[test]
    fn restarted_daemon_claims_admission_from_a_dead_owner() {
        with_isolated_home(|_| {
            // A generation that died while still owning admission and one job.
            let dead = state("DEAD", "127.0.0.1:1001");
            seed(&dead).expect("seed dead owner");
            record_job("job-dead", "DEAD").expect("record dead owner job");

            let restarted = state("LIVE", "127.0.0.1:1002");
            seed(&restarted).expect("seed is a no-op on an existing registry");
            assert_eq!(
                admitting().expect("admitting").expect("owner").lease_id,
                "DEAD"
            );

            let moved =
                claim_admission_from_dead_owner(&restarted, |endpoint| endpoint.lease_id == "DEAD")
                    .expect("claim");
            assert!(moved);
            assert_eq!(
                admitting().expect("admitting").expect("owner").lease_id,
                "LIVE"
            );
            // The dead generation keeps its job routing for recovery.
            assert_eq!(
                endpoint_for_job("job-dead")
                    .expect("route")
                    .expect("dead")
                    .lease_id,
                "DEAD"
            );

            // Idempotent once the serving daemon owns admission.
            assert!(!claim_admission_from_dead_owner(&restarted, |_| true).expect("again"));
        });
    }

    #[test]
    fn explicit_router_roots_keep_identical_controller_job_ids_separate() {
        with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("router roots");
            let left_root = parent.path().join("left");
            let right_root = parent.path().join("right");
            fs::create_dir_all(&left_root).expect("left router");
            fs::create_dir_all(&right_root).expect("right router");

            {
                let _left = EnvVarGuard::set(DAEMON_ROUTER_DIR_ENV, &left_root);
                let left = state("left-generation", "127.0.0.1:19401");
                seed(&left).expect("seed left router");
                record_job("same-controller-job", "left-generation").expect("left job owner");
            }
            {
                let _right = EnvVarGuard::set(DAEMON_ROUTER_DIR_ENV, &right_root);
                let right = state("right-generation", "127.0.0.1:19402");
                seed(&right).expect("seed right router");
                record_job("same-controller-job", "right-generation").expect("right job owner");

                assert_eq!(
                    endpoint_for_job("same-controller-job")
                        .expect("ambient right route")
                        .expect("right route")
                        .address,
                    "127.0.0.1:19402"
                );
                assert_eq!(
                    endpoint_for_job_in_router_dir("same-controller-job", &left_root)
                        .expect("explicit left route")
                        .expect("left route")
                        .address,
                    "127.0.0.1:19401"
                );
                assert_eq!(
                    endpoint_for_job_in_router_dir("same-controller-job", &right_root)
                        .expect("explicit right route")
                        .expect("right route")
                        .address,
                    "127.0.0.1:19402"
                );
            }
        });
    }

    #[test]
    fn rooted_controller_job_client_cancels_through_the_selected_router() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        with_isolated_home(|_| {
            let left_listener = TcpListener::bind("127.0.0.1:0").expect("left endpoint");
            let right_listener = TcpListener::bind("127.0.0.1:0").expect("right endpoint");
            let left_address = left_listener.local_addr().unwrap();
            let right_address = right_listener.local_addr().unwrap();
            let job_id = uuid::Uuid::new_v4();
            let serve = |listener: TcpListener| {
                listener
                    .set_nonblocking(true)
                    .expect("nonblocking test endpoint");
                std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    let (mut stream, _) = loop {
                        match listener.accept() {
                            Ok(connection) => break connection,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                return String::new();
                            }
                            Err(error) => panic!("accept controller request: {error}"),
                        }
                    };
                    let mut request = [0_u8; 4096];
                    let count = stream.read(&mut request).expect("read controller request");
                    let response = serde_json::json!({
                        "success": true,
                        "data": {
                            "body": {
                                "success": true,
                                "job": {
                                "id": job_id,
                                "operation": "agent_task",
                                "status": "queued",
                                "created_at_ms": 1,
                                "updated_at_ms": 2,
                                "event_count": 1
                                }
                            }
                        }
                    })
                    .to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response.len(),
                        response
                    )
                    .expect("write controller response");
                    String::from_utf8_lossy(&request[..count]).into_owned()
                })
            };
            let left_server = serve(left_listener);
            let right_server = serve(right_listener);

            let parent = tempfile::tempdir().expect("router roots");
            let left_config = parent.path().join("left-config");
            let right_config = parent.path().join("right-config");
            let left_root = left_config.join("daemon");
            let right_root = right_config.join("daemon");
            fs::create_dir_all(&left_root).expect("left router");
            fs::create_dir_all(&right_root).expect("right router");
            {
                let _left = EnvVarGuard::set(DAEMON_ROUTER_DIR_ENV, &left_root);
                let left = state("left-controller", &left_address.to_string());
                seed(&left).expect("seed left generation");
                record_job(&job_id.to_string(), "left-controller").expect("left job owner");
            }
            {
                let _right = EnvVarGuard::set(DAEMON_ROUTER_DIR_ENV, &right_root);
                let right = state("right-controller", &right_address.to_string());
                seed(&right).expect("seed ambient right generation");
                record_job(&job_id.to_string(), "right-controller").expect("right job owner");

                let client = super::super::LocalControllerJobClient::connect_existing_job_in_root(
                    &job_id.to_string(),
                    left_config.as_path(),
                )
                .expect("connect using left lifecycle root");
                let job = client
                    .cancel(&job_id.to_string(), "rooted test cancellation")
                    .expect("cancel through left root");
                assert_eq!(job.status, crate::api_jobs::JobStatus::Queued);
            }

            let left_request = left_server.join().expect("left server");
            assert!(left_request.starts_with(&format!("POST /controller/jobs/{job_id}/cancel ")));
            assert!(
                right_server.join().expect("right server").is_empty(),
                "ambient same-ID route must not receive rooted cancellation"
            );
        });
    }

    #[test]
    fn live_admission_owner_is_never_displaced() {
        with_isolated_home(|_| {
            let live = state("A", "127.0.0.1:1001");
            seed(&live).expect("seed A");
            let candidate = state("B", "127.0.0.1:1002");
            let moved = claim_admission_from_dead_owner(&candidate, |_| false).expect("claim");
            assert!(!moved);
            assert_eq!(
                admitting().expect("admitting").expect("owner").lease_id,
                "A"
            );
            assert!(endpoint_for_lease("B").expect("lookup").is_none());
        });
    }

    #[test]
    fn no_registry_means_nothing_to_claim() {
        with_isolated_home(|_| {
            let serving = state("ONLY", "127.0.0.1:1001");
            assert!(!claim_admission_from_dead_owner(&serving, |_| true).expect("claim"));
        });
    }

    #[test]
    fn handoff_keeps_a_job_routable_while_b_admits_new_work() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            record_job("job-a", "A").expect("record A job");
            let b = state("B", "127.0.0.1:1002");
            activate(&b).expect("activate B");
            record_job("job-b", "B").expect("record B job");

            assert_eq!(
                endpoint_for_job("job-a")
                    .expect("route A")
                    .expect("A")
                    .address,
                a.address
            );
            assert_eq!(
                endpoint_for_job("job-b")
                    .expect("route B")
                    .expect("B")
                    .address,
                b.address
            );
            assert!(is_draining("A").expect("A drains"));
            // A failed stop retains the drained generation's job routes for
            // status and the next lifecycle pass rather than losing its
            // recovery identity.
            assert!(
                reconcile_drained_generations("B", |_| Err(Error::internal_unexpected(
                    "stop failed"
                )))
                .is_err()
            );
            assert_eq!(
                endpoint_for_job("job-a")
                    .expect("retain A route")
                    .expect("A")
                    .address,
                a.address
            );
            reconcile_drained_generations("B", |endpoint| {
                assert_eq!(endpoint.lease_id, "A");
                Ok(())
            })
            .expect("retire stopped A");
            assert!(endpoint_for_job("job-a").expect("retired A").is_none());
            assert_eq!(admitting().expect("admitting").expect("B").lease_id, "B");
        });
    }

    #[test]
    fn finds_a_generation_by_its_exact_lease() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            let b = state("B", "127.0.0.1:1002");
            activate(&b).expect("activate B");

            assert_eq!(
                endpoint_for_lease("A")
                    .expect("look up A")
                    .expect("A endpoint")
                    .state_dir,
                a.state_path
                    .strip_suffix("/state.json")
                    .expect("state path has filename")
            );
            assert!(endpoint_for_lease("missing")
                .expect("look up missing")
                .is_none());
        });
    }

    #[test]
    fn retired_generation_preserves_exact_pruned_controller_terminal_custody() {
        with_isolated_home(|home| {
            use crate::api_jobs::{
                ControllerJobState, ControllerJobSubmissionOutcome, JobStatus, JobStore,
            };
            let source_dir = home.path().join("old-generation");
            let mut old = state("retained-old", "127.0.0.1:1001");
            old.state_path = source_dir.join("state.json").display().to_string();
            seed(&old).unwrap();
            let store =
                JobStore::open_without_reconciliation(source_dir.join("jobs.json")).unwrap();
            let ControllerJobSubmissionOutcome::Submitted(id) = store
                .admit_controller_job(
                    "controller.retained-loop".to_string(),
                    "retained-loop:exact-generation".to_string(),
                    ControllerJobState {
                        job_type: "retained-loop".to_string(),
                        version: 1,
                        request: serde_json::json!({}),
                        public_request: serde_json::json!({}),
                        request_digest: "original".to_string(),
                        active_idempotency_key: None,
                        linked_durable_run_id: None,
                        checkpoint: None,
                        cancellation_requested: false,
                        cancellation_reason: None,
                        execution_claim_id: None,
                        recovery_attempted: false,
                    },
                )
                .unwrap()
            else {
                panic!("new job")
            };
            record_job(&id.to_string(), &old.lease_id).unwrap();
            store.start_controller_execution(id).unwrap();
            store
                .fail_controller_error(
                    id,
                    "original consumer failure".to_string(),
                    serde_json::json!({}),
                )
                .unwrap();
            store
                .prune_terminal_controller_jobs("retained-loop", 1, &[id])
                .unwrap();
            // The terminal job is already compacted out of the generation's
            // store, so nothing pins the drained generation and no registry
            // sweep is required before retirement.
            activate(&state("retained-new", "127.0.0.1:1002")).unwrap();
            reconcile_drained_generations("retained-new", |_| Ok(())).unwrap();
            assert!(endpoint_for_job(&id.to_string()).unwrap().is_none());
            std::fs::remove_dir_all(&source_dir).unwrap();
            assert_eq!(
                terminal_controller_job(id).unwrap().unwrap().status,
                JobStatus::Failed
            );
            let serving = super::super::DaemonControllerJobService::new(JobStore::default());
            assert_eq!(
                serving.status(&id.to_string()).unwrap().status,
                JobStatus::Failed,
                "HTTP/control-plane actions in a successor daemon retain original terminal custody"
            );
            assert!(terminal_controller_job(uuid::Uuid::new_v4())
                .unwrap()
                .is_none());
        });
    }

    #[test]
    fn a_blocked_retirement_does_not_strand_other_idle_generations() {
        with_isolated_home(|_| {
            seed(&state("A", "127.0.0.1:1001")).expect("seed A");
            let finished_a = admitted_job("A", "blocked-retirement-finished");
            open_generation_store("A")
                .fail(finished_a, "finished")
                .expect("finish A job");
            activate(&state("B", "127.0.0.1:1002")).expect("activate B");
            activate(&state("C", "127.0.0.1:1003")).expect("activate C");
            let active_c = admitted_job("C", "blocked-retirement-live");
            activate(&state("D", "127.0.0.1:1004")).expect("activate D");

            // One durable store is shared by every generation here; the
            // owner filter keeps each job attributed to exactly one
            // generation.
            let registry = read_registry().expect("read registry").expect("registry");
            assert_eq!(
                generation_active_jobs(&registry.generations, "A").expect("derived A"),
                0,
                "a terminal job no longer pins its generation"
            );
            assert_eq!(
                generation_active_jobs(&registry.generations, "C").expect("derived C"),
                1
            );

            let stopped = std::sync::Mutex::new(Vec::new());
            let result = reconcile_drained_generations("D", |endpoint| {
                stopped.lock().unwrap().push(endpoint.lease_id.clone());
                if endpoint.lease_id == "A" {
                    Err(Error::internal_unexpected("exact A lease is blocked"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err(), "retain the original retirement diagnostic");
            assert_eq!(*stopped.lock().unwrap(), ["A", "B"]);
            assert!(endpoint_for_lease("B").unwrap().is_none());
            assert!(endpoint_for_lease("A").unwrap().is_some());
            assert_eq!(
                endpoint_for_job(&finished_a.to_string())
                    .unwrap()
                    .unwrap()
                    .lease_id,
                "A"
            );
            assert_eq!(
                endpoint_for_job(&active_c.to_string())
                    .unwrap()
                    .unwrap()
                    .lease_id,
                "C"
            );
            assert!(super::super::lifetime::owns_global_work("D"));
            assert!(!super::super::lifetime::owns_global_work("A"));
            assert!(!super::super::lifetime::owns_global_work("C"));

            reconcile_drained_generations("D", |_| Ok(()))
                .expect("retry only the retained idle generation");
            assert!(endpoint_for_lease("A").unwrap().is_none());
            assert!(endpoint_for_lease("C").unwrap().is_some());
        });
    }

    #[test]
    fn concurrent_job_ownership_writes_preserve_every_admission() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(17));
            let workers = (0..16)
                .map(|index| {
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        record_job(&format!("job-{index}"), "A")
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            for worker in workers {
                worker.join().expect("join writer").expect("record job");
            }
            for index in 0..16 {
                assert_eq!(
                    endpoint_for_job(&format!("job-{index}"))
                        .expect("read owner")
                        .expect("owner")
                        .lease_id,
                    "A"
                );
            }
        });
    }

    #[test]
    fn lost_admission_response_retry_keeps_the_original_generation_owner() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            // The server committed this binding, but the client lost the response.
            record_job("job-a", "A").expect("first server admission");
            let b = state("B", "127.0.0.1:1002");
            activate(&b).expect("rotate to B");
            // A retry reaches B. Idempotent recording must preserve A rather than
            // silently re-home the already durable job.
            record_job("job-a", "B").expect("retry admission");
            assert_eq!(
                endpoint_for_job("job-a")
                    .expect("route job")
                    .expect("owner")
                    .lease_id,
                "A"
            );
        });
    }

    #[test]
    fn startup_rebuild_survives_jobs_owned_by_retired_generations() {
        with_isolated_home(|_| {
            // The registry holds one live generation, while durable jobs still
            // name the long-gone daemons that admitted them.
            let live = state("live", "127.0.0.1:1001");
            seed(&live).expect("seed live generation");

            rebuild_job_owner("historical-job", "retired-generation")
                .expect("a retired generation must not fail startup recovery");

            // The retired generation owns nothing to route, and the live
            // generation is left free to admit work.
            assert!(endpoint_for_job("historical-job")
                .expect("route historical job")
                .is_none());
            assert_eq!(
                admitting().expect("admitting").expect("live").lease_id,
                "live"
            );

            // A job belonging to a generation that is still registered is
            // rebuilt and routable.
            rebuild_job_owner("live-job", "live").expect("rebuild live job");
            assert_eq!(
                endpoint_for_job("live-job")
                    .expect("route live job")
                    .expect("live owner")
                    .lease_id,
                "live"
            );
        });
    }

    #[test]
    fn proven_dead_transfer_moves_only_recovered_jobs_once() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            let b = state("B", "127.0.0.1:1002");
            seed(&a).expect("seed A");
            // A restart in place shares the dead lease's durable store with
            // its replacement: both registered generations name the same
            // state directory, so jobs must count for exactly one owner.
            let store = open_generation_store("A");
            let recovered = store.create("proven-dead-recovered");
            let terminal = store.create("proven-dead-terminal");
            let other = store.create("proven-dead-other");
            record_job(&recovered.id.to_string(), "A").expect("record recovered job");
            record_job(&terminal.id.to_string(), "A").expect("record terminal job");
            record_job(&other.id.to_string(), "A").expect("record other job");
            store
                .fail(terminal.id, "finished before the daemon died")
                .expect("mark terminal job");

            transfer_proven_dead_job(&recovered.id.to_string(), "A", &b)
                .expect("transfer recovered job");
            transfer_proven_dead_job(&terminal.id.to_string(), "A", &b)
                .expect("transfer terminal job");
            // The same recovery can be retried without re-counting the job.
            transfer_proven_dead_job(&recovered.id.to_string(), "A", &b).expect("repeat transfer");

            assert_eq!(
                endpoint_for_job(&recovered.id.to_string())
                    .expect("route recovered")
                    .expect("replacement endpoint")
                    .lease_id,
                "B"
            );
            assert_eq!(
                endpoint_for_job(&other.id.to_string())
                    .expect("route unrelated")
                    .expect("original endpoint")
                    .lease_id,
                "A"
            );
            let registry = read_registry().expect("read registry").expect("registry");
            // The transferred job counts for the replacement generation only,
            // with no double count in the shared store, and the terminal job
            // counts for nobody.
            assert_eq!(
                generation_active_jobs(&registry.generations, "A").expect("derived A"),
                1
            );
            assert_eq!(
                generation_active_jobs(&registry.generations, "B").expect("derived B"),
                1
            );
            // Every registry write refreshes the write-only compat counters
            // from the owner ledger: A keeps its retained route and B owns
            // both transferred jobs.
            assert_eq!(registry.generations.generations["A"].active_jobs, 1);
            assert_eq!(registry.generations.generations["B"].active_jobs, 2);

            // Each generation still owns durable work, so neither retires.
            assert!(
                !retire_exact_dead_generation("A", state_dir_of(&a)).expect("consult registry"),
                "the retained job keeps A busy"
            );
            assert!(
                !retire_exact_dead_generation("B", state_dir_of(&b)).expect("consult registry"),
                "the transferred job keeps its replacement busy"
            );

            let before = registry.clone();
            assert!(transfer_proven_dead_job(&other.id.to_string(), "wrong", &b).is_err());
            assert_eq!(
                read_registry().expect("read unchanged registry"),
                Some(before)
            );
        });
    }

    #[test]
    fn replacement_generations_are_siblings_under_the_stable_router_root() {
        with_isolated_home(|home| {
            let router = home.path().join("stable-router");
            let first_generation = router.join("generations").join("first");
            let _router_env = EnvVarGuard::set(DAEMON_ROUTER_DIR_ENV, &router);
            std::env::set_var(crate::paths::DAEMON_STATE_DIR_ENV, &first_generation);

            let second_generation = generation_state_dir().expect("allocate second generation");
            std::env::set_var(crate::paths::DAEMON_STATE_DIR_ENV, &second_generation);
            let third_generation = generation_state_dir().expect("allocate third generation");

            assert_eq!(
                second_generation.parent(),
                Some(router.join("generations").as_path())
            );
            assert_eq!(
                third_generation.parent(),
                Some(router.join("generations").as_path())
            );
            assert!(!third_generation.starts_with(&second_generation));
        });
    }

    /// #14706: a replacement daemon must admit work whose request still names
    /// the retired lease, instead of failing with "job owner was not present
    /// in the generation registry".
    #[test]
    fn an_admission_named_for_a_retired_lease_rehomes_to_the_serving_generation() {
        with_isolated_home(|_| {
            let serving = state("serving", "127.0.0.1:1101");
            seed(&serving).expect("seed serving");
            record_job("existing", "serving").expect("record existing job");

            record_job_for_admission("stale-request", "retired", &serving)
                .expect("stale lease is not an invariant violation");

            assert_eq!(
                endpoint_for_job("stale-request")
                    .expect("route stale request")
                    .expect("owner")
                    .lease_id,
                "serving"
            );
            // Existing durable ownership is untouched and the serving
            // generation stays the admission owner.
            assert_eq!(
                endpoint_for_job("existing")
                    .expect("route existing")
                    .expect("owner")
                    .lease_id,
                "serving"
            );
        });
    }

    /// The strict historical spelling keeps refusing leases that are simply
    /// absent: only the daemon's own admission path re-homes.
    #[test]
    fn record_job_still_refuses_an_unregistered_owner() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1102");
            seed(&a).expect("seed A");
            assert!(record_job("unrouted", "missing").is_err());
        });
    }

    #[test]
    fn exact_dead_generation_retirement_removes_only_that_registry_entry() {
        with_isolated_home(|home| {
            let state_dir = home.path().join("exact-generation");
            std::fs::create_dir_all(&state_dir).expect("create exact state dir");
            let exact_dir = state_dir.display().to_string();
            mutate_registry(|registry| {
                let mut generations = RollingGenerations::new(
                    "exact",
                    LocalDaemonEndpoint {
                        lease_id: "exact".to_string(),
                        address: "127.0.0.1:1103".to_string(),
                        state_dir: exact_dir.clone(),
                        build_identity: "test".to_string(),
                    },
                );
                generations.begin(
                    "other",
                    LocalDaemonEndpoint {
                        lease_id: "other".to_string(),
                        address: "127.0.0.1:1104".to_string(),
                        state_dir: format!("{exact_dir}/../other"),
                        build_identity: "test".to_string(),
                    },
                );
                *registry = Some(LocalDaemonGenerationRegistry {
                    schema: SCHEMA.to_string(),
                    generations,
                });
                Ok(())
            })
            .expect("register two generations");
            let store =
                crate::api_jobs::JobStore::open_without_reconciliation(state_dir.join("jobs.json"))
                    .expect("open exact store")
                    .with_daemon_lease("exact".to_string());
            let busy = store.create("exact-retirement");
            record_job(&busy.id.to_string(), "exact").expect("record busy job");

            // A generation with durable work is never retired.
            assert!(!retire_exact_dead_generation("exact", exact_dir.as_str())
                .expect("consult registry"));
            // A mismatched frame never retires another endpoint.
            assert!(!retire_exact_dead_generation("exact", "/nowhere").expect("consult registry"));
            assert!(read_registry().expect("read").is_some_and(|registry| {
                registry.generations.generations.contains_key("exact")
            }));

            store
                .fail(busy.id, "finished")
                .expect("terminalize the exact store job");
            drop(store);
            assert!(retire_exact_dead_generation("exact", exact_dir.as_str())
                .expect("consult registry"));
            let registry = read_registry().expect("read registry").expect("registry");
            assert!(!registry.generations.generations.contains_key("exact"));
            assert!(registry.generations.generations.contains_key("other"));
            assert!(endpoint_for_job(&busy.id.to_string())
                .expect("route")
                .is_none());
        });
    }

    /// Offline CLI reconciliation terminalizes jobs in the generation's own
    /// store without any daemon running, so no registry counter was ever
    /// decremented. The derived count must still release the dead generation
    /// for retirement.
    #[test]
    fn offline_reconciliation_releases_the_dead_generation_for_retirement() {
        with_isolated_home(|_| {
            use crate::api_jobs::JobStatus;
            let offline = state("offline", "127.0.0.1:1001");
            seed(&offline).expect("seed offline generation");
            let job = admitted_job("offline", "offline-reconciled");

            // No daemon runs here: the CLI reconciliation terminalizes the
            // job in its own store, and nothing ever touched the registry.
            let store = open_generation_store("offline");
            let reconciled = store
                .reconcile_dead_daemon_lease_jobs("offline")
                .expect("offline reconciliation");
            assert_eq!(reconciled.matching_job_ids, vec![job]);
            assert_eq!(
                store.get(job).expect("job").status,
                JobStatus::Failed,
                "offline reconciliation terminalizes the job in its own store"
            );
            drop(store);

            assert!(
                retire_exact_dead_generation("offline", state_dir_of(&offline))
                    .expect("consult registry"),
                "a dead generation whose jobs were reconciled offline must retire"
            );
            assert!(endpoint_for_lease("offline").expect("lookup").is_none());
            assert!(endpoint_for_job(&job.to_string()).expect("route").is_none());
        });
    }

    /// With the orchestration interval disabled no registry sweep ever runs;
    /// a terminal job in the generation's own store must still let the
    /// drained generation retire.
    #[test]
    fn a_terminal_job_releases_its_generation_without_an_orchestration_tick() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            let job = admitted_job("A", "tickless-terminal");
            open_generation_store("A")
                .fail(job, "finished")
                .expect("terminalize the store job");

            activate(&state("B", "127.0.0.1:1002")).expect("activate B");
            reconcile_drained_generations("B", |endpoint| {
                assert_eq!(endpoint.lease_id, "A");
                Ok(())
            })
            .expect("a terminal store job alone releases the drained generation");
            assert!(endpoint_for_lease("A").expect("lookup").is_none());
            assert!(endpoint_for_job(&job.to_string()).expect("route").is_none());
        });
    }

    /// A live job blocks retirement on both paths, and an unreadable store
    /// counts as busy so the generation is never retired on incomplete
    /// evidence.
    #[test]
    fn a_live_job_and_an_unreadable_store_block_retirement() {
        with_isolated_home(|home| {
            let a = state("A", "127.0.0.1:1001");
            seed(&a).expect("seed A");
            let live = admitted_job("A", "still-running");
            let mut b = state("B", "127.0.0.1:1002");
            b.state_path = home
                .path()
                .join("unreadable-generation")
                .join("state.json")
                .display()
                .to_string();
            activate(&b).expect("activate B");
            let unreadable_dir = home.path().join("unreadable-generation");
            std::fs::create_dir_all(&unreadable_dir).expect("unreadable generation dir");
            std::fs::write(unreadable_dir.join("jobs.json"), b"{ not json")
                .expect("write unreadable store");
            activate(&state("C", "127.0.0.1:1003")).expect("activate C");

            assert!(
                !retire_exact_dead_generation("A", state_dir_of(&a)).expect("consult registry"),
                "a live job blocks exact retirement"
            );
            assert!(
                !retire_exact_dead_generation("B", state_dir_of(&b)).expect("consult registry"),
                "an unreadable store counts as busy"
            );
            let stopped = std::sync::Mutex::new(Vec::new());
            reconcile_drained_generations("C", |endpoint| {
                stopped.lock().unwrap().push(endpoint.lease_id.clone());
                Ok(())
            })
            .expect("busy generations are skipped, not stopped");
            assert!(stopped.lock().unwrap().is_empty());
            assert!(endpoint_for_lease("A").expect("lookup").is_some());
            assert!(endpoint_for_lease("B").expect("lookup").is_some());

            // Once the job turns terminal in the store, A retires while the
            // unreadable generation stays registered.
            open_generation_store("A")
                .fail(live, "finished")
                .expect("terminalize the live job");
            assert!(
                retire_exact_dead_generation("A", state_dir_of(&a)).expect("consult registry"),
                "a terminal job releases its generation"
            );
            assert!(endpoint_for_lease("A").expect("lookup").is_none());
            assert!(endpoint_for_lease("B").expect("lookup").is_some());
        });
    }

    /// A legacy registry still carrying `completed_jobs` loads unchanged;
    /// the removed set is ignored on read and never written back.
    #[test]
    fn a_legacy_registry_with_completed_jobs_still_loads() {
        with_isolated_home(|_| {
            seed(&state("legacy", "127.0.0.1:1001")).expect("seed legacy generation");
            let path = registry_path().expect("registry path");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).expect("read registry"))
                    .expect("parse registry");
            value["completed_jobs"] = serde_json::json!(["finished-job"]);
            std::fs::write(
                &path,
                serde_json::to_vec_pretty(&value).expect("serialize legacy registry"),
            )
            .expect("write legacy registry");

            let registry = read_registry()
                .expect("a legacy registry with completed_jobs still loads")
                .expect("registry");
            assert!(registry.generations.generations.contains_key("legacy"));
            assert_eq!(
                admitting().expect("admitting").expect("owner").lease_id,
                "legacy"
            );

            record_job("legacy-job", "legacy").expect("record owner");
            let rewritten: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).expect("reread registry"))
                    .expect("reparse registry");
            assert!(
                rewritten.get("completed_jobs").is_none(),
                "the retired set must not be written back"
            );
        });
    }
}
