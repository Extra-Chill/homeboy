//! Stable routing state for local daemon generations.

use std::collections::BTreeSet;
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
    #[serde(default)]
    completed_jobs: BTreeSet<String>,
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
    let path = registry_path()?;
    match fs::read(&path) {
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
    if let Some(registry) = registry.as_ref() {
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
                completed_jobs: BTreeSet::new(),
            });
        }
        Ok(())
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

pub(super) fn endpoint_for_job(job_id: &str) -> Result<Option<LocalDaemonEndpoint>> {
    Ok(read_registry()?.and_then(|registry| {
        registry.generations.job_owner(job_id).and_then(|owner| {
            registry
                .generations
                .generations
                .get(owner)
                .map(|entry| entry.endpoint.clone())
        })
    }))
}

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

        let completed = registry.completed_jobs.contains(job_id);
        registry
            .generations
            .job_owners
            .insert(job_id.to_string(), replacement_lease_id.clone());
        if !completed {
            let old = registry
                .generations
                .generations
                .get_mut(proven_dead_lease_id)
                .expect("proven-dead generation was validated");
            old.active_jobs = old.active_jobs.saturating_sub(1);
            let replacement = registry
                .generations
                .generations
                .get_mut(replacement_lease_id)
                .expect("replacement generation was inserted or found");
            replacement.active_jobs += 1;
        }
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

pub(super) fn mark_job_terminal(job_id: &str) -> Result<()> {
    mutate_registry(|registry| {
        let Some(registry) = registry.as_mut() else {
            return Ok(());
        };
        if !registry.completed_jobs.insert(job_id.to_string()) {
            return Ok(());
        }
        if let Some(owner) = registry.generations.job_owner(job_id).map(str::to_string) {
            if let Some(generation) = registry.generations.generations.get_mut(&owner) {
                generation.active_jobs = generation.active_jobs.saturating_sub(1);
            }
        }
        Ok(())
    })
}

pub(super) fn reconcile_drained_generations(
    serving_lease_id: &str,
    stop: impl Fn(&LocalDaemonEndpoint) -> Result<()>,
) -> Result<()> {
    let endpoints = read_registry()?
        .into_iter()
        .flat_map(|registry| registry.generations.generations.into_iter())
        .filter_map(|(lease_id, generation)| {
            (lease_id != serving_lease_id
                && generation.drain_state == RollingDrainState::Draining
                && generation.active_jobs == 0)
                .then_some((lease_id, generation.endpoint))
        })
        .collect::<Vec<_>>();
    for (lease_id, endpoint) in endpoints {
        stop(&endpoint)?;
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
                            && generation.active_jobs == 0
                    });
            if can_retire {
                registry.generations.generations.remove(&lease_id);
                registry
                    .generations
                    .job_owners
                    .retain(|_, owner| owner != &lease_id);
                registry
                    .completed_jobs
                    .retain(|job_id| registry.generations.job_owners.contains_key(job_id));
            }
            Ok(())
        })?;
    }
    Ok(())
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
            mark_job_terminal("job-a").expect("mark A terminal");
            // A failed stop retains the terminal job's owner for status and the
            // next lifecycle pass rather than losing its recovery identity.
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
    fn proven_dead_transfer_moves_only_recovered_jobs_once() {
        with_isolated_home(|_| {
            let a = state("A", "127.0.0.1:1001");
            let b = state("B", "127.0.0.1:1002");
            seed(&a).expect("seed A");
            record_job("recovered", "A").expect("record recovered job");
            record_job("terminal", "A").expect("record terminal job");
            record_job("other", "A").expect("record other job");
            mark_job_terminal("terminal").expect("mark terminal job");

            transfer_proven_dead_job("recovered", "A", &b).expect("transfer recovered job");
            transfer_proven_dead_job("terminal", "A", &b).expect("transfer terminal job");
            // The same recovery can be retried without moving active counts again.
            transfer_proven_dead_job("recovered", "A", &b).expect("repeat transfer");

            assert_eq!(
                endpoint_for_job("recovered")
                    .expect("route recovered")
                    .expect("replacement endpoint")
                    .lease_id,
                "B"
            );
            assert_eq!(
                endpoint_for_job("other")
                    .expect("route unrelated")
                    .expect("original endpoint")
                    .lease_id,
                "A"
            );
            let registry = read_registry().expect("read registry").expect("registry");
            assert_eq!(registry.generations.generations["A"].active_jobs, 1);
            assert_eq!(registry.generations.generations["B"].active_jobs, 1);

            let before = registry.clone();
            assert!(transfer_proven_dead_job("other", "wrong", &b).is_err());
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
}
