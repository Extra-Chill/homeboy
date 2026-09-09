//! Stable routing state for local daemon generations.

use std::fs;
use std::path::{Path, PathBuf};

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
    fs::write(&temporary, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", path.display())),
        )
    })
}

pub(super) fn seed(state: &DaemonState) -> Result<()> {
    if read_registry()?.is_some() {
        return Ok(());
    }
    let endpoint = LocalDaemonEndpoint::from_state(state);
    write_registry(&LocalDaemonGenerationRegistry {
        schema: SCHEMA.to_string(),
        generations: RollingGenerations::new(endpoint.lease_id.clone(), endpoint),
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
    let Some(mut registry) = read_registry()? else {
        return Err(Error::internal_unexpected(
            "local daemon admitted a job without a generation registry",
        ));
    };
    if !registry.generations.admit_job_for(lease_id, job_id)
        && registry.generations.job_owner(job_id).is_none()
    {
        return Err(Error::internal_unexpected(
            "local daemon job owner was not present in the generation registry",
        ));
    }
    write_registry(&registry)
}

pub(super) fn activate(state: &DaemonState) -> Result<()> {
    let endpoint = LocalDaemonEndpoint::from_state(state);
    let mut registry = read_registry()?.ok_or_else(|| {
        Error::internal_unexpected("local daemon rotation has no seeded generation registry")
    })?;
    if registry
        .generations
        .begin(endpoint.lease_id.clone(), endpoint)
        == RollingStart::Start
    {
        // `begin` leaves candidates draining. Only a caller that has checked the
        // published lease and exact build may expose it to admissions.
    }
    registry.generations.activate(&state.lease_id);
    write_registry(&registry)
}

pub(super) fn complete_job(job_id: &str) -> Result<Option<LocalDaemonEndpoint>> {
    let Some(mut registry) = read_registry()? else {
        return Ok(None);
    };
    let endpoint = registry
        .generations
        .job_owner(job_id)
        .and_then(|owner| registry.generations.generations.get(owner))
        .map(|entry| entry.endpoint.clone());
    let retired = registry.generations.complete_job(job_id);
    write_registry(&registry)?;
    Ok(retired.then_some(endpoint).flatten())
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
            assert!(complete_job("job-a").expect("complete A").is_some());
            assert!(endpoint_for_job("job-a").expect("retired A").is_none());
            assert_eq!(admitting().expect("admitting").expect("B").lease_id, "B");
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
