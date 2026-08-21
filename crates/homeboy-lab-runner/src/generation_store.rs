use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;
use homeboy_core::process::{
    process_identity_state_with_start_identity, process_start_identity, ProcessIdentityState,
    ProcessStartIdentity,
};
use homeboy_core::server::SshClient;

use crate::rolling_generation::RollingResultOwnerRetirement;
use crate::{
    RollingGenerations, RunnerDaemonGenerationStatus, RunnerGenerationJobOwners, RunnerSession,
};

#[derive(Debug, Clone)]
pub(crate) struct AdmissionFence {
    pub generation: String,
    pub active_job_count: usize,
}

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

fn recovery_lock_path(runner_id: &str, generation: &str) -> Result<PathBuf> {
    let generation = generation
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join(format!("recovery-{generation}.lock")))
}

fn pending_replacement_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("pending-replacement.json"))
}

fn replacement_operation_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("replacement-operation.json"))
}

fn rejected_replacement_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("rejected-replacements")
        .join(format!("{}.json", uuid::Uuid::new_v4())))
}

fn superseded_replacement_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("superseded-replacements")
        .join(format!("{}.json", uuid::Uuid::new_v4())))
}

fn admission_reservation_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::runner_sessions_dir()?
        .join(runner_id)
        .join("admission-mutation-reservation.json"))
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AdmissionReservation {
    operation_id: String,
    owner_pid: u32,
    owner_start_identity: ProcessStartIdentity,
    created_at: String,
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReplacementOperation {
    runner_id: String,
    operation_id: String,
    #[serde(default)]
    replay_command: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(serde::Serialize)]
struct RejectedReplacementEvidence<'a> {
    schema: &'static str,
    runner_id: &'a str,
    operation_id: &'a str,
    kind: &'a str,
    replay_command: Option<&'a str>,
    rejected_at: String,
    exit_code: i32,
    timed_out: bool,
    stdout: &'a str,
    stderr: &'a str,
}

#[derive(serde::Serialize)]
struct SupersededReplacementEvidence<'a> {
    schema: &'static str,
    runner_id: &'a str,
    operation_id: &'a str,
    previous_kind: &'a str,
    previous_replay_command: &'a str,
    replacement_kind: &'a str,
    replacement_replay_command: &'a str,
    superseded_at: String,
}

pub(crate) fn write_durable_json<T: serde::Serialize>(
    path: &std::path::Path,
    value: &T,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal_unexpected("journal has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("journal"),
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize replacement journal".to_string()),
        )
    })?;
    let mut file = File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", temporary.display())),
        )
    })?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write {}", temporary.display())),
            )
        })?;
    std::fs::rename(&temporary, path).map_err(|error| {
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

/// Serialize read-modify-write registry updates independently for each runner.
/// A status reconciliation and `/exec` acceptance can arrive concurrently.
fn with_lock<T>(
    lock_path: PathBuf,
    runner_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
    const LOCK_RETRY: Duration = Duration::from_millis(25);
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
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("open generation lock".to_string()))
        })?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("lock generation registry".to_string()),
                ));
            }
            if Instant::now() >= deadline {
                let mut error = Error::internal_io(
                    format!(
                        "timed out after {}ms waiting for runner generation registry lock",
                        LOCK_TIMEOUT.as_millis()
                    ),
                    Some("lock generation registry".to_string()),
                );
                error.details = serde_json::json!({
                    "kind": "runner_generation_lock_timeout",
                    "runner_id": runner_id,
                    "timeout_ms": LOCK_TIMEOUT.as_millis(),
                });
                error.retryable = Some(true);
                return Err(error);
            }
            std::thread::sleep(LOCK_RETRY);
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

fn with_registry_lock<T>(runner_id: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    with_lock(
        path(runner_id)?.with_extension("lock"),
        runner_id,
        operation,
    )
}

/// Cross-process lifecycle serialization for one runner. Operations that mutate
/// a controller session must use this same lock as generation rotation.
pub(crate) fn with_runner_registry_lock<T>(
    runner_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_registry_lock(runner_id, operation)
}

pub(crate) fn with_generation_recovery_lock<T>(
    runner_id: &str,
    generation: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_lock(
        recovery_lock_path(runner_id, generation)?,
        runner_id,
        operation,
    )
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

/// Read generation state for an observation without acquiring a writer lock or
/// repairing a legacy registry. Status must describe legacy state, not migrate
/// it: a concurrent writer owns that repair through the normal mutation path.
fn read_projection(
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
    match serde_json::from_value::<GenerationRegistry<RunnerSession>>(value.clone()) {
        Ok(registry) if registry.runner_id == runner_id => Ok(Some(registry.generations)),
        Ok(registry) => Err(Error::config_invalid_value(
            "runner_id",
            Some(registry.runner_id),
            format!("generation registry must match runner-scoped path `{runner_id}`"),
        )),
        // This is the precise legacy shape the mutating reader repairs. Keep it
        // readable here so status remains a projection even while a writer owns
        // the registry lock.
        Err(_) => Ok(Some(
            serde_json::from_value::<RollingGenerations<RunnerSession>>(value)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?,
        )),
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

/// Reserve daemon mutation under the registry lock, then release it for remote
/// I/O. Job admission observes the durable reservation and fails closed until
/// the mutation completes or its reservation is cleared.
pub(crate) fn with_admission_fence<T>(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    operation_name: &str,
    operation: impl FnOnce(Option<&AdmissionFence>) -> Result<T>,
) -> Result<T> {
    let (reservation, fence) = with_registry_lock(runner_id, || {
        if let Some(reservation) = active_admission_reservation_locked(runner_id)? {
            return Err(admission_reservation_error(runner_id, &reservation));
        }
        let generations = read_locked(runner_id, legacy)?;
        let reservation = AdmissionReservation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            owner_pid: std::process::id(),
            owner_start_identity: process_start_identity(std::process::id())
                .map_err(Error::internal_unexpected)?
                .ok_or_else(|| {
                    Error::internal_unexpected(
                        "current process exited while reserving daemon mutation",
                    )
                })?,
            created_at: Utc::now().to_rfc3339(),
            operation: operation_name.to_string(),
            generation: legacy.map(legacy_generation),
        };
        write_durable_json(&admission_reservation_path(runner_id)?, &reservation)?;
        let fence = generations.as_ref().and_then(|generations| {
            generations
                .generations
                .iter()
                .find(|(_, entry)| entry.active_jobs > 0)
                .map(|(generation, entry)| AdmissionFence {
                    generation: generation.clone(),
                    active_job_count: entry.active_jobs,
                })
        });
        Ok((reservation, fence))
    })?;
    let result = operation(fence.as_ref());
    let clear = clear_owned_admission_reservation(runner_id, &reservation);
    match (result, clear) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Err(error), Err(_)) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Read a reservation while holding the registry lock. A process that has
/// exited or whose PID was reused cannot still own a remote mutation, so its
/// durable reservation is reclaimed before a new owner proceeds.
fn active_admission_reservation_locked(runner_id: &str) -> Result<Option<AdmissionReservation>> {
    let path = admission_reservation_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let reservation: AdmissionReservation =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    match process_identity_state_with_start_identity(
        reservation.owner_pid,
        None,
        Some(&reservation.owner_start_identity),
    ) {
        ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch => {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", path.display())),
                )
            })?;
            Ok(None)
        }
        ProcessIdentityState::Live | ProcessIdentityState::Unverifiable => Ok(Some(reservation)),
    }
}

fn clear_owned_admission_reservation(
    runner_id: &str,
    reservation: &AdmissionReservation,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let path = admission_reservation_path(runner_id)?;
        if !path.exists() {
            return Ok(());
        }
        let current: AdmissionReservation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        if current.operation_id == reservation.operation_id
            && current.owner_pid == reservation.owner_pid
            && current.owner_start_identity == reservation.owner_start_identity
        {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", path.display())),
                )
            })?;
        }
        Ok(())
    })
}

fn admission_reservation_error(runner_id: &str, reservation: &AdmissionReservation) -> Error {
    let mut error = Error::validation_invalid_argument(
        "runner_mutation_reservation",
        format!(
            "runner `{runner_id}` daemon mutation `{}` for generation `{}` is still owned by PID {}; retry admission after it completes or run `homeboy runner reconcile {runner_id}` to recover a dead owner",
            reservation.operation,
            reservation.generation.as_deref().unwrap_or("unknown"),
            reservation.owner_pid,
        ),
        Some(runner_id.to_string()),
        Some(vec![format!("homeboy runner reconcile {runner_id}")]),
    )
    .with_retryable(true);
    error.details["reservation_operation"] = serde_json::json!(reservation.operation);
    error.details["reservation_generation"] = serde_json::json!(reservation.generation);
    error.details["reservation_created_at"] = serde_json::json!(reservation.created_at);
    error
}

/// A recovery-created daemon is durable before a controller can authenticate
/// its tunnel. Retain its exact coordinates so an interruption cannot fall
/// back to the superseded generation or start a competing daemon.
pub(crate) fn pending_replacement(runner_id: &str) -> Result<Option<RunnerSession>> {
    let path = pending_replacement_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let session: RunnerSession = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if session.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "pending_replacement.runner_id",
            Some(session.runner_id),
            format!("pending replacement must match runner `{runner_id}`"),
        ));
    }
    Ok(Some(session))
}

/// Persist the controller id before invoking a remote lifecycle mutation. The
/// daemon uses this exact id as its durable startup token/receipt key.
pub(crate) fn replacement_operation(runner_id: &str) -> Result<String> {
    with_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        if path.exists() {
            let operation: ReplacementOperation =
                serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
                })?)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
            if operation.runner_id != runner_id {
                return Err(Error::config_invalid_value(
                    "replacement_operation.runner_id",
                    Some(operation.runner_id),
                    "replacement operation must match its runner",
                ));
            }
            return Ok(operation.operation_id);
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        write_durable_json(
            &path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: operation_id.clone(),
                replay_command: None,
                kind: None,
            },
        )?;
        Ok(operation_id)
    })
}

pub(crate) fn replacement_operation_replay(runner_id: &str) -> Result<Option<(String, String)>> {
    let path = replacement_operation_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let operation: ReplacementOperation =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if operation.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "replacement_operation.runner_id",
            Some(operation.runner_id),
            "replacement operation must match its runner",
        ));
    }
    Ok(operation.kind.zip(operation.replay_command))
}

pub(crate) fn record_replacement_operation_replay(
    runner_id: &str,
    kind: &str,
    command: &str,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        let mut operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        operation.replay_command = Some(command.to_string());
        operation.kind = Some(kind.to_string());
        write_durable_json(&path, &operation)
    })
}

/// Bind candidate reconciliation to the current replacement identity. An
/// explicit operator recovery may supersede only `ensure-running`: that command
/// can create one of the candidates reconciliation is responsible for, and
/// replaying it first would reproduce the conflict the operator selected this
/// recovery to resolve.
pub(crate) fn record_unleased_candidate_reconciliation_replay(
    runner_id: &str,
    command: &str,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        let mut operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        match (
            operation.kind.as_deref(),
            operation.replay_command.as_deref(),
        ) {
            (Some("unleased-candidates"), Some(existing)) if existing == command => return Ok(()),
            (Some("ensure-running"), Some(previous_command)) => {
                write_durable_json(
                    &superseded_replacement_path(runner_id)?,
                    &SupersededReplacementEvidence {
                        schema: "homeboy/runner-replacement-operation-supersession/v1",
                        runner_id,
                        operation_id: &operation.operation_id,
                        previous_kind: "ensure-running",
                        previous_replay_command: previous_command,
                        replacement_kind: "unleased-candidates",
                        replacement_replay_command: command,
                        superseded_at: Utc::now().to_rfc3339(),
                    },
                )?;
            }
            (None, None) => {}
            _ => {
                return Err(Error::internal_unexpected(
                    "replacement operation cannot transition to unleased candidate reconciliation",
                ));
            }
        }
        operation.kind = Some("unleased-candidates".to_string());
        operation.replay_command = Some(command.to_string());
        write_durable_json(&path, &operation)
    })
}

/// An acknowledged `applied: false` reconciliation is terminal: the remote
/// inspected the candidate set and declined to mutate it. Rotate the operation
/// identity rather than replaying that stale observation on a later connect.
pub(crate) fn terminalize_unleased_candidate_reconciliation(runner_id: &str) -> Result<()> {
    with_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        let operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        if operation.runner_id != runner_id
            || operation.kind.as_deref() != Some("unleased-candidates")
            || operation.replay_command.is_none()
        {
            return Err(Error::internal_unexpected(
                "refusing to terminalize a replacement operation that is not an immutable unleased candidate reconciliation",
            ));
        }
        write_durable_json(
            &path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: uuid::Uuid::new_v4().to_string(),
                replay_command: None,
                kind: None,
            },
        )
    })
}

/// Update an operation journal while the caller holds this runner's registry
/// lock through [`with_admission_fence`].
pub(crate) fn record_replacement_operation_replay_locked(
    runner_id: &str,
    kind: &str,
    command: &str,
) -> Result<()> {
    let path = replacement_operation_path(runner_id)?;
    let mut operation: ReplacementOperation =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    operation.replay_command = Some(command.to_string());
    operation.kind = Some(kind.to_string());
    write_durable_json(&path, &operation)
}

pub(crate) fn record_pending_replacement(runner_id: &str, session: &RunnerSession) -> Result<()> {
    with_registry_lock(runner_id, || {
        write_durable_json(&pending_replacement_path(runner_id)?, session)
    })
}

/// Retire an interrupted replacement only after the caller has re-probed the
/// remote lease and proved that these recorded coordinates are no longer a
/// publishable daemon. A later attempt receives a new operation identity.
pub(crate) fn retire_pending_replacement(runner_id: &str) -> Result<String> {
    retire_replacement(runner_id)
}

fn retire_replacement(runner_id: &str) -> Result<String> {
    with_registry_lock(runner_id, || {
        let operation_id = uuid::Uuid::new_v4().to_string();
        // Publish the successor receipt before releasing the old coordinates.
        // A crash can therefore leave both records, but never neither record.
        write_durable_json(
            &replacement_operation_path(runner_id)?,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: operation_id.clone(),
                replay_command: None,
                kind: None,
            },
        )?;
        let pending_path = pending_replacement_path(runner_id)?;
        if pending_path.exists() {
            std::fs::remove_file(&pending_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", pending_path.display())),
                )
            })?;
        }
        Ok(operation_id)
    })
}

/// Retire a remote policy or validation refusal after preserving the exact
/// terminal response. Unlike an interrupted mutation, this operation cannot
/// become publishable by replaying the same authority.
pub(crate) fn retire_rejected_state_loss_replacement(
    runner_id: &str,
    output: &homeboy_core::server::CommandOutput,
) -> Result<()> {
    with_registry_lock(runner_id, || {
        let operation_path = replacement_operation_path(runner_id)?;
        let operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&operation_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read {}", operation_path.display())),
                )
            })?)
            .map_err(|error| {
                Error::config_invalid_json(operation_path.display().to_string(), error)
            })?;
        if operation.runner_id != runner_id || operation.kind.as_deref() != Some("state-loss") {
            return Err(Error::internal_unexpected(
                "refusing to retire a replacement operation that is not the rejected state-loss recovery",
            ));
        }
        let evidence_path = rejected_replacement_path(runner_id)?;
        write_durable_json(
            &evidence_path,
            &RejectedReplacementEvidence {
                schema: "homeboy/runner-rejected-replacement/v1",
                runner_id,
                operation_id: &operation.operation_id,
                kind: "state-loss",
                replay_command: operation.replay_command.as_deref(),
                rejected_at: Utc::now().to_rfc3339(),
                exit_code: output.exit_code,
                timed_out: output.timed_out,
                stdout: &output.stdout,
                stderr: &output.stderr,
            },
        )?;
        write_durable_json(
            &operation_path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: uuid::Uuid::new_v4().to_string(),
                replay_command: None,
                kind: None,
            },
        )?;
        let pending_path = pending_replacement_path(runner_id)?;
        if pending_path.exists() {
            std::fs::remove_file(&pending_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", pending_path.display())),
                )
            })?;
        }
        Ok(())
    })
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
    Ok(status_admission_projection(runner_id, legacy)?.0)
}

/// Return durable job identities grouped by generation. These identities can
/// be deduplicated with live daemon jobs; persisted counters cannot.
pub(crate) fn status_job_owners(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<Vec<RunnerGenerationJobOwners>> {
    Ok(status_admission_projection(runner_id, legacy)?.1)
}

/// Capture the complete admission ledger from one persisted generation read.
/// Inventory and durable job ownership must describe the same generation state.
pub(crate) fn status_admission_projection(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<(
    Vec<RunnerDaemonGenerationStatus>,
    Vec<RunnerGenerationJobOwners>,
)> {
    Ok(read_projection(runner_id, legacy)?.map_or_else(
        || (Vec::new(), Vec::new()),
        |generations| {
            let inventory = generations
                .generations
                .iter()
                .map(|(generation, entry)| RunnerDaemonGenerationStatus {
                    job_owner_count: generations
                        .job_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, generation, &entry.endpoint)
                        })
                        .count(),
                    run_owner_count: generations
                        .run_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, generation, &entry.endpoint)
                        })
                        .count(),
                    artifact_owner_count: generations
                        .artifact_owners
                        .values()
                        .filter(|owner| {
                            owner_matches_generation(owner, generation, &entry.endpoint)
                        })
                        .count(),
                    admission_owner: *generation == generations.admission_owner,
                    generation: generation.clone(),
                    drain_state: entry.drain_state,
                    active_job_count: entry.active_jobs,
                    observed_active_job_count: entry.observed_active_jobs,
                    active_job_count_authoritative: entry.observed_active_jobs.is_some(),
                    homeboy_build_identity: entry.endpoint.homeboy_build_identity.clone(),
                    remote_daemon_lease_id: entry.endpoint.remote_daemon_lease_id.clone(),
                    remote_daemon_address: entry.endpoint.remote_daemon_address.clone(),
                    local_url: entry.endpoint.local_url.clone(),
                })
                .collect();
            let owners = generations
                .generations
                .iter()
                .map(|(generation, entry)| RunnerGenerationJobOwners {
                    generation: generation.clone(),
                    job_ids: job_owner_ids_for(&generations, generation, &entry.endpoint),
                })
                .collect();
            (inventory, owners)
        },
    ))
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
        .filter(|(_, owner)| owner_matches_generation(owner, generation, endpoint))
        .map(|(job_id, _)| job_id.clone())
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

/// Remove a registry only after its caller has obtained authoritative remote
/// proof that every recorded direct daemon generation is dead and idle. Check
/// the exact lease set again under the registry lock so a concurrent reconnect
/// cannot lose a newly admitted generation.
pub(crate) fn tombstone_dead_direct_generations(
    runner_id: &str,
    expected_leases: &[String],
) -> Result<()> {
    let expected_leases = expected_leases
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    with_registry_lock(runner_id, || {
        let Some(generations) = read_locked(runner_id, None)? else {
            return Ok(());
        };
        let current_leases = generations
            .generations
            .values()
            .map(|entry| {
                (entry.endpoint.mode == crate::RunnerTunnelMode::DirectSsh)
                    .then_some(entry.endpoint.remote_daemon_lease_id.as_deref())
                    .flatten()
                    .filter(|lease| !lease.is_empty())
            })
            .collect::<Option<std::collections::BTreeSet<_>>>();
        if current_leases.as_ref() != Some(&expected_leases) {
            return Err(Error::validation_invalid_argument(
                "runner",
                "runner generation registry changed while dead-daemon evidence was being reconciled; retained all generations",
                Some(runner_id.to_string()),
                None,
            ));
        }
        let path = path(runner_id)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("delete {}", path.display())),
                )
            })?;
        }
        // The registry only persists a numeric local tunnel PID, not a process
        // identity. It may have been reused since this stale generation was
        // recorded, so convergence must not signal it.
        Ok(())
    })
}

trait GenerationEndpointOperations {
    /// Terminal linked handoffs can survive a controller restart in a daemon's
    /// job store. Settle them before treating its active count as ownership.
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool;
    fn active_jobs(&self, session: &RunnerSession) -> Option<usize>;
    fn stop(&self, session: &RunnerSession) -> bool;
    fn terminate_tunnel(&self, session: &RunnerSession);
}

struct HttpGenerationEndpointOperations {
    client: reqwest::blocking::Client,
}

struct SshGenerationEndpointOperations<'a> {
    client: &'a SshClient,
}

struct FallbackGenerationEndpointOperations<'a, Primary, Fallback> {
    primary: &'a Primary,
    fallback: &'a Fallback,
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
        let health = health.json::<serde_json::Value>().ok()?;
        daemon_health_data(&health)
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

    fn terminate_tunnel(&self, session: &RunnerSession) {
        crate::connection::terminate_tunnel_if_owned(session);
    }
}

impl SshGenerationEndpointOperations<'_> {
    fn endpoint_url(session: &RunnerSession, path: &str) -> Option<String> {
        if session.mode != crate::RunnerTunnelMode::DirectSsh {
            return None;
        }
        let address = session.remote_daemon_address.as_deref()?;
        let address = address.parse::<std::net::SocketAddr>().ok()?;
        if !address.ip().is_loopback() {
            return None;
        }
        Some(format!("http://{address}{path}"))
    }

    fn request(
        &self,
        method: &str,
        session: &RunnerSession,
        path: &str,
        body: Option<&str>,
    ) -> Option<String> {
        let url = Self::endpoint_url(session, path)?;
        let mut command = format!(
            "curl --fail --silent --show-error --max-time 5 --request {} {}",
            shell::quote_arg(method),
            shell::quote_arg(&url),
        );
        if let Some(body) = body {
            command.push_str(&format!(
                " --header 'Content-Type: application/json' --data {}",
                shell::quote_arg(body),
            ));
        }
        let output = self
            .client
            .execute_with_timeout(&command, Duration::from_secs(10));
        output.success.then_some(output.stdout)
    }

    fn authenticated_health(&self, session: &RunnerSession) -> Option<serde_json::Value> {
        let output = self.request("GET", session, "/health", None)?;
        let health = serde_json::from_str::<serde_json::Value>(&output).ok()?;
        let health = daemon_health_data(&health);
        Self::health_matches_session(session, health).then(|| health.clone())
    }

    fn health_matches_session(session: &RunnerSession, health: &serde_json::Value) -> bool {
        let Some(expected_lease) = session.remote_daemon_lease_id.as_deref() else {
            return false;
        };
        let Some(expected_pid) = session.remote_daemon_pid.map(u64::from) else {
            return false;
        };
        let observed_lease = health
            .pointer("/lease/lease_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                health
                    .pointer("/freshness/lease_id")
                    .and_then(serde_json::Value::as_str)
            });
        observed_lease == Some(expected_lease)
            && health.get("pid").and_then(serde_json::Value::as_u64) == Some(expected_pid)
    }
}

fn daemon_health_data(health: &serde_json::Value) -> &serde_json::Value {
    health.get("data").unwrap_or(health)
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::{RunnerSessionRole, RunnerTunnelMode};

    fn session(lease_id: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "shared-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("unreachable-lab".to_string()),
            controller_id: Some("shared-controller".to_string()),
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:49152".to_string()),
            local_port: Some(49153),
            local_url: Some("http://127.0.0.1:49153".to_string()),
            tunnel_pid: Some(u32::MAX),
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: Some(4242),
            remote_daemon_lease_id: Some(lease_id.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+shared".to_string()),
            connected_at: "2026-08-05T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn shared_status_projects_a_large_legacy_registry_without_repairing_it() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut generations = RollingGenerations::new("lease-000", session("lease-000"));
            for index in 1..=70 {
                let generation = format!("lease-{index:03}");
                generations.begin(generation.clone(), session(&generation));
            }
            write("shared-lab", &generations).expect("write generation registry");
            let path = path("shared-lab").expect("generation registry path");
            let mut legacy: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read registry"))
                    .expect("parse registry");
            legacy
                .as_object_mut()
                .expect("registry object")
                .remove("runner_id");
            let legacy = serde_json::to_vec_pretty(&legacy).expect("serialize legacy registry");
            std::fs::write(&path, &legacy).expect("write legacy registry");

            let started = Instant::now();
            let projection = status_projection("shared-lab", Some(&session("lease-000")))
                .expect("project legacy generation registry");

            assert_eq!(projection.len(), 71);
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(std::fs::read(&path).expect("read after status"), legacy);
        });
    }
}

impl GenerationEndpointOperations for SshGenerationEndpointOperations<'_> {
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
        self.authenticated_health(session).is_some()
            && self
                .request("POST", session, "/jobs/reconcile-terminal", None)
                .is_some()
    }

    fn active_jobs(&self, session: &RunnerSession) -> Option<usize> {
        self.authenticated_health(session)?
            .pointer("/freshness/active_jobs")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
    }

    fn stop(&self, session: &RunnerSession) -> bool {
        let Some(lease_id) = session.remote_daemon_lease_id.as_deref() else {
            return false;
        };
        if self.authenticated_health(session).is_none() {
            return false;
        }
        let body = serde_json::json!({ "lease_id": lease_id, "force": false }).to_string();
        self.request("POST", session, "/lifecycle/stop", Some(&body))
            .is_some()
    }

    fn terminate_tunnel(&self, session: &RunnerSession) {
        crate::connection::terminate_tunnel_if_owned(session);
    }
}

impl<Primary, Fallback> GenerationEndpointOperations
    for FallbackGenerationEndpointOperations<'_, Primary, Fallback>
where
    Primary: GenerationEndpointOperations,
    Fallback: GenerationEndpointOperations,
{
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
        self.primary.reconcile_terminal_jobs(session)
            || self.fallback.reconcile_terminal_jobs(session)
    }

    fn active_jobs(&self, session: &RunnerSession) -> Option<usize> {
        self.primary
            .active_jobs(session)
            .or_else(|| self.fallback.active_jobs(session))
    }

    fn stop(&self, session: &RunnerSession) -> bool {
        self.primary.stop(session) || self.fallback.stop(session)
    }

    fn terminate_tunnel(&self, session: &RunnerSession) {
        self.primary.terminate_tunnel(session);
    }
}

/// Reconciliation is intentionally fail-closed: an unreachable draining
/// endpoint remains recorded and routable. Each reachable draining daemon first
/// settles terminal durable handoffs, then its own health response becomes the
/// authoritative count for that generation.
#[derive(Debug, Default)]
pub(crate) struct GenerationReconcileResult {
    pub retired_generation_ids: Vec<String>,
}

pub(crate) fn reconcile(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
) -> Result<GenerationReconcileResult> {
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

/// Reconcile generations through their controller-local tunnels, falling back
/// to the same recorded loopback daemon endpoints over the trusted SSH runner.
/// This restores observability after an old generation's local tunnel exits
/// without weakening the daemon's terminal-job, zero-active-job, or stop gates.
pub(crate) fn reconcile_with_ssh(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    ssh_client: &SshClient,
) -> Result<GenerationReconcileResult> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build generation reconcile client: {error}"))
        })?;
    let local = HttpGenerationEndpointOperations { client };
    let remote = SshGenerationEndpointOperations { client: ssh_client };
    reconcile_with(
        runner_id,
        legacy,
        &FallbackGenerationEndpointOperations {
            primary: &local,
            fallback: &remote,
        },
    )
}

fn reconcile_with(
    runner_id: &str,
    legacy: Option<&RunnerSession>,
    operations: &impl GenerationEndpointOperations,
) -> Result<GenerationReconcileResult> {
    let Some(generations) = read(runner_id, legacy)? else {
        return Ok(GenerationReconcileResult::default());
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
                operations.terminate_tunnel(&session);
                Some(generation)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let retired_generation_ids = with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, legacy)? else {
            return Ok(Vec::new());
        };
        let mut retired = Vec::new();
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
                retired.push(generation.clone());
            }
        }
        write(runner_id, &generations)?;
        Ok(retired)
    })?;
    Ok(GenerationReconcileResult {
        retired_generation_ids,
    })
}

pub(crate) fn record_job(runner_id: &str, session: &RunnerSession, job_id: &str) -> Result<()> {
    with_registry_lock(runner_id, || {
        if let Some(reservation) = active_admission_reservation_locked(runner_id)? {
            return Err(admission_reservation_error(runner_id, &reservation));
        }
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

/// Replace the local endpoint for a job's already-owned generation after an
/// authenticated reattachment. Unlike admission, this cannot change where new
/// work is sent.
pub(crate) fn record_reconnected_job_owner(
    runner_id: &str,
    session: &RunnerSession,
    job_id: &str,
) -> Result<RunnerSession> {
    let lease_id = session
        .remote_daemon_lease_id
        .as_deref()
        .ok_or_else(|| Error::internal_unexpected("reconnected daemon session has no lease ID"))?;
    with_registry_lock(runner_id, || {
        let Some(mut generations) = read_locked(runner_id, Some(session))? else {
            return Err(Error::validation_invalid_argument(
                "runner",
                "runner has no durable daemon generation binding for this job",
                Some(runner_id.to_string()),
                None,
            ));
        };
        let owner = generations.job_owner(job_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "job_id",
                "runner has no durable daemon generation binding for this job",
                Some(job_id.to_string()),
                None,
            )
        })?;
        let Some(generation) = generations
            .generations
            .iter()
            .find_map(|(generation, entry)| {
                (generation == owner
                    || entry.endpoint.remote_daemon_lease_id.as_deref() == Some(owner))
                .then_some(generation.clone())
            })
        else {
            return Err(Error::internal_unexpected(
                "job owner does not resolve to a persisted daemon generation",
            ));
        };
        let entry = generations
            .generations
            .get_mut(&generation)
            .expect("job owner generation was resolved");
        if entry.endpoint.remote_daemon_lease_id.as_deref() != Some(lease_id) {
            return Err(Error::validation_invalid_argument(
                "runner",
                "reattached daemon lease does not match the durable job owner",
                Some(runner_id.to_string()),
                None,
            ));
        }
        let replaced = std::mem::replace(&mut entry.endpoint, session.clone());
        write(runner_id, &generations)?;
        Ok(replaced)
    })
}

/// Promote B only after `connect_remote_daemon` has authenticated its lease,
/// PID, version, and build identity. Keeping the pending record until this
/// locked transaction commits makes every interruption retry B first.
pub(crate) fn promote_pending_replacement(runner_id: &str, session: &RunnerSession) -> Result<()> {
    let generation = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        Error::internal_unexpected("authenticated daemon session has no lease ID")
    })?;
    with_registry_lock(runner_id, || {
        if let Some(pending) = pending_replacement(runner_id)? {
            if pending.remote_daemon_lease_id.as_deref() != Some(generation)
                || pending.remote_daemon_pid != session.remote_daemon_pid
                || pending.remote_daemon_address != session.remote_daemon_address
            {
                return Err(Error::validation_invalid_argument(
                    "pending_replacement",
                    "authenticated daemon does not match the pending replacement; refusing authority promotion",
                    Some(runner_id.to_string()),
                    None,
                ));
            }
        }
        record_authenticated_admission_locked(runner_id, session, generation)?;
        let path = pending_replacement_path(runner_id)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", path.display())),
                )
            })?;
        }
        let operation_path = replacement_operation_path(runner_id)?;
        if operation_path.exists() {
            std::fs::remove_file(&operation_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", operation_path.display())),
                )
            })?;
        }
        Ok(())
    })
}

fn record_authenticated_admission_locked(
    runner_id: &str,
    session: &RunnerSession,
    generation: &str,
) -> Result<()> {
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
    reconcile(runner_id, legacy).map(|_| ())
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

    #[test]
    fn durable_json_write_overwrites_existing_file() {
        let directory = tempfile::tempdir().expect("create journal directory");
        let path = directory.path().join("journal.json");
        std::fs::write(&path, "stale payload that is longer than the replacement")
            .expect("write stale journal");

        write_durable_json(&path, &json!({ "state": "current" })).expect("overwrite journal");

        assert_eq!(
            std::fs::read_to_string(path).expect("read replacement journal"),
            "{\n  \"state\": \"current\"\n}"
        );
    }

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
            tunnel_process_start_identity: None,
            proxy_forward: None,
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

    fn reservation(
        operation: &str,
        pid: u32,
        identity: ProcessStartIdentity,
    ) -> AdmissionReservation {
        AdmissionReservation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            owner_pid: pid,
            owner_start_identity: identity,
            created_at: "2026-08-03T00:00:00Z".to_string(),
            operation: operation.to_string(),
            generation: Some("lease-a".to_string()),
        }
    }

    #[test]
    fn live_reservation_blocks_admission_with_retryable_reconcile_action() {
        test_support::with_isolated_home(|_| {
            let current_identity = process_start_identity(std::process::id())
                .expect("inspect test process")
                .expect("test process is live");
            write_durable_json(
                &admission_reservation_path("runner-a").expect("reservation path"),
                &reservation("ensure_remote_daemon", std::process::id(), current_identity),
            )
            .expect("write live reservation");

            let error = record_job("runner-a", &session("lease-a", "daemon-a", None), "job-a")
                .expect_err("live mutation blocks admission");

            assert_eq!(error.code.as_str(), "validation.invalid_argument");
            assert_eq!(error.retryable, Some(true));
            assert_eq!(error.details["field"], "runner_mutation_reservation");
            assert_eq!(
                error.details["reservation_operation"],
                "ensure_remote_daemon"
            );
            assert_eq!(
                error.details["tried"],
                serde_json::json!(["homeboy runner reconcile runner-a"])
            );
        });
    }

    #[test]
    fn failed_mutation_clears_its_owned_reservation_before_returning() {
        test_support::with_isolated_home(|_| {
            let error = with_admission_fence("runner-a", None, "ensure_remote_daemon", |_| {
                Err::<(), _>(Error::internal_unexpected("remote mutation failed"))
            })
            .expect_err("operation failure is returned");
            assert_eq!(error.message, "remote mutation failed");
            assert!(
                !admission_reservation_path("runner-a")
                    .expect("reservation path")
                    .exists(),
                "an error path must not strand its reservation"
            );
            record_job("runner-a", &session("lease-a", "daemon-a", None), "job-a")
                .expect("admission proceeds after failed mutation cleanup");
        });
    }

    #[test]
    fn concurrent_admission_observes_mutation_reservation_without_waiting_for_remote_work() {
        test_support::with_isolated_home(|_| {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let holder_barrier = std::sync::Arc::clone(&barrier);
            let holder = std::thread::spawn(move || {
                with_admission_fence("runner-a", None, "ensure_remote_daemon", |_| {
                    holder_barrier.wait();
                    holder_barrier.wait();
                    Ok(())
                })
            });

            barrier.wait();
            let error = record_job("runner-a", &session("lease-a", "daemon-a", None), "job-a")
                .expect_err("admission races a durable mutation reservation");
            assert_eq!(error.retryable, Some(true));
            assert_eq!(
                error.details["reservation_operation"],
                "ensure_remote_daemon"
            );
            barrier.wait();
            holder
                .join()
                .expect("mutation owner thread")
                .expect("mutation succeeds");
            record_job("runner-a", &session("lease-a", "daemon-a", None), "job-a")
                .expect("admission succeeds after reservation release");
        });
    }

    #[test]
    fn dead_reservation_is_reclaimed_before_admission() {
        test_support::with_isolated_home(|_| {
            let mut owner = std::process::Command::new("sh")
                .args(["-c", "sleep 60"])
                .spawn()
                .expect("start reservation owner");
            let identity = process_start_identity(owner.id())
                .expect("inspect child")
                .expect("child is live");
            owner.kill().expect("stop reservation owner");
            owner.wait().expect("reap reservation owner");
            write_durable_json(
                &admission_reservation_path("runner-a").expect("reservation path"),
                &reservation("recover_missing_lease_state", owner.id(), identity),
            )
            .expect("write dead reservation");

            record_job("runner-a", &session("lease-a", "daemon-a", None), "job-a")
                .expect("dead owner reservation is reclaimed");

            assert!(!admission_reservation_path("runner-a")
                .expect("reservation path")
                .exists());
        });
    }

    #[test]
    fn retiring_an_unpublishable_replacement_releases_its_endpoint_and_operation_identity() {
        test_support::with_isolated_home(|_| {
            let first_operation = replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay("runner-a", "ensure-running", "replay")
                .expect("replay journal");
            record_pending_replacement("runner-a", &session("lease-dead", "127.0.0.1", None))
                .expect("pending coordinates");

            let next_operation =
                retire_pending_replacement("runner-a").expect("retire dead pending replacement");

            assert!(pending_replacement("runner-a")
                .expect("read pending")
                .is_none());
            assert!(replacement_operation_replay("runner-a")
                .expect("read replay")
                .is_none());
            assert_ne!(next_operation, first_operation);
            assert_eq!(
                replacement_operation("runner-a").expect("new operation"),
                next_operation
            );
        });
    }

    #[test]
    fn explicit_candidate_reconciliation_supersedes_ensure_running_with_evidence() {
        test_support::with_isolated_home(|_| {
            let operation_id = replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay(
                "runner-a",
                "ensure-running",
                "homeboy daemon ensure-running --replacement-operation-id operation-a",
            )
            .expect("ensure-running replay");

            let reconciliation = "homeboy daemon reconcile-unleased-candidates --apply --replacement-operation-id operation-a";
            record_unleased_candidate_reconciliation_replay("runner-a", reconciliation)
                .expect("explicit reconciliation supersedes ensure-running");
            record_unleased_candidate_reconciliation_replay("runner-a", reconciliation)
                .expect("same reconciliation replay is idempotent");

            assert_eq!(
                replacement_operation("runner-a").expect("preserved operation"),
                operation_id
            );
            assert_eq!(
                replacement_operation_replay("runner-a").expect("replacement replay"),
                Some((
                    "unleased-candidates".to_string(),
                    reconciliation.to_string()
                ))
            );
            let evidence_dir = paths::runner_sessions_dir()
                .expect("runner sessions")
                .join("runner-a")
                .join("superseded-replacements");
            let evidence = std::fs::read_dir(evidence_dir)
                .expect("supersession evidence")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("evidence entries");
            assert_eq!(evidence.len(), 1, "idempotent replay writes one transition");
            let evidence: serde_json::Value = serde_json::from_slice(
                &std::fs::read(evidence[0].path()).expect("read supersession evidence"),
            )
            .expect("supersession evidence JSON");
            assert_eq!(evidence["operation_id"], operation_id);
            assert_eq!(evidence["previous_kind"], "ensure-running");
            assert_eq!(evidence["replacement_kind"], "unleased-candidates");
        });
    }

    #[test]
    fn legacy_replacement_journal_remains_replayable_after_rebinding() {
        test_support::with_isolated_home(|_| {
            let path = replacement_operation_path("runner-a").expect("journal path");
            write_durable_json(
                &path,
                &serde_json::json!({
                    "runner_id": "runner-a",
                    "operation_id": "legacy-operation",
                }),
            )
            .expect("write legacy journal");

            assert_eq!(
                replacement_operation("runner-a").expect("read operation"),
                "legacy-operation"
            );
            assert!(replacement_operation_replay("runner-a")
                .expect("read legacy replay")
                .is_none());
            record_replacement_operation_replay(
                "runner-a",
                "ensure-running",
                "/selected/homeboy daemon ensure-running --replacement-operation-id legacy-operation --addr 127.0.0.1:0",
            )
            .expect("rebind legacy journal");
            assert_eq!(
                replacement_operation_replay("runner-a").expect("read rebound replay"),
                Some((
                    "ensure-running".to_string(),
                    "/selected/homeboy daemon ensure-running --replacement-operation-id legacy-operation --addr 127.0.0.1:0".to_string(),
                ))
            );
        });
    }

    #[derive(Default)]
    struct FakeEndpointOperations {
        active_jobs: RefCell<std::collections::BTreeMap<String, usize>>,
        terminal_reconcile_failures: RefCell<std::collections::BTreeSet<String>>,
        stop_failures: RefCell<std::collections::BTreeSet<String>>,
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
            let lease_id = session.remote_daemon_lease_id.clone().expect("lease");
            self.stopped_leases.borrow_mut().push(lease_id.clone());
            !self.stop_failures.borrow().contains(&lease_id)
        }

        fn terminate_tunnel(&self, session: &RunnerSession) {
            if let Some(pid) = session.tunnel_pid {
                self.terminated_pids.borrow_mut().push(pid);
            }
        }
    }

    #[test]
    fn concurrent_status_projections_preserve_stale_direct_generation_state_without_operations() {
        test_support::with_isolated_home(|_| {
            let draining = session("lease-draining", "daemon-draining", Some(101));
            let active = session("lease-active", "daemon-active", Some(202));
            record_job("runner-a", &draining, "draining-job").expect("record draining job");
            activate(
                "runner-a",
                &draining,
                "lease-active".to_string(),
                active.clone(),
                &["draining-job".to_string()],
            )
            .expect("activate current generation");
            let registry_path = path("runner-a").expect("registry path");
            let before = std::fs::read(&registry_path).expect("snapshot durable state");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

            let observers = (0..2)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let active = active.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        status_projection("runner-a", Some(&active)).expect("status projection")
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            for observer in observers {
                let projection = observer.join().expect("status observer");
                assert_eq!(projection.len(), 2);
            }

            assert_eq!(
                std::fs::read(registry_path).expect("read durable state"),
                before
            );
        });
    }

    #[test]
    fn endpoint_fallback_retires_only_the_authoritatively_idle_draining_generation() {
        test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            record_job("runner-a", &a, "job-a").expect("record A job");
            activate(
                "runner-a",
                &a,
                "lease-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("activate B");

            let local = FakeEndpointOperations::default();
            local
                .terminal_reconcile_failures
                .borrow_mut()
                .insert("lease-a".to_string());
            local
                .stop_failures
                .borrow_mut()
                .insert("lease-a".to_string());
            local
                .active_jobs
                .borrow_mut()
                .insert("lease-b".to_string(), 0);
            let remote = FakeEndpointOperations::default();
            remote
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 0);

            reconcile_with(
                "runner-a",
                Some(&b),
                &FallbackGenerationEndpointOperations {
                    primary: &local,
                    fallback: &remote,
                },
            )
            .expect("reconcile through fallback");

            let registry = persisted_registry("runner-a");
            assert_eq!(registry["admission_owner"], "lease-b");
            assert!(registry["generations"].get("lease-a").is_none());
            assert!(registry["generations"].get("lease-b").is_some());
            assert!(registry["job_owners"].get("job-a").is_none());
            assert_eq!(
                remote.stopped_leases.borrow().as_slice(),
                ["lease-a"],
                "the fallback stops only the drained endpoint"
            );
            assert_eq!(local.terminated_pids.borrow().as_slice(), [101]);
            assert!(remote.terminated_pids.borrow().is_empty());
        });
    }

    #[test]
    fn reconciled_zero_for_admission_owner_releases_fence_but_retains_job_identities() {
        test_support::with_isolated_home(|_| {
            let current = session("lease-current", "daemon-current", Some(202));
            record_job("runner-a", &current, "job-a").expect("record first job");
            record_job("runner-a", &current, "job-b").expect("record second job");
            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-current".to_string(), 0);

            reconcile_with("runner-a", Some(&current), &operations)
                .expect("reconcile authoritative remote zero");

            let projection = status_projection("runner-a", Some(&current)).expect("projection");
            assert_eq!(projection[0].active_job_count, 0);
            assert_eq!(projection[0].observed_active_job_count, Some(0));
            assert_eq!(
                status_job_owners("runner-a", Some(&current)).expect("owners")[0].job_ids,
                ["job-a", "job-b"],
                "zero live work settles the active count without discarding durable ownership"
            );
            with_admission_fence("runner-a", Some(&current), "connect", |fence| {
                assert!(fence.is_none(), "authoritative zero must permit reconnect");
                Ok(())
            })
            .expect("released fence");
        });
    }

    #[test]
    fn ssh_fallback_authenticates_the_exact_generation_lease_and_pid() {
        let expected = session("lease-a", "127.0.0.1", Some(101));
        assert!(SshGenerationEndpointOperations::health_matches_session(
            &expected,
            &json!({
                "pid": 42,
                "lease": { "lease_id": "lease-a" },
                "freshness": { "active_jobs": 0 },
            }),
        ));
        assert!(!SshGenerationEndpointOperations::health_matches_session(
            &expected,
            &json!({ "pid": 42, "lease": { "lease_id": "lease-reused" } }),
        ));
        assert!(!SshGenerationEndpointOperations::health_matches_session(
            &expected,
            &json!({ "pid": 43, "lease": { "lease_id": "lease-a" } }),
        ));
        let wrapped = json!({
            "success": true,
            "data": {
                "pid": 42,
                "lease": null,
                "freshness": { "lease_id": "lease-a", "active_jobs": 0 },
            },
        });
        assert!(SshGenerationEndpointOperations::health_matches_session(
            &expected,
            daemon_health_data(&wrapped),
        ));
        assert_eq!(
            daemon_health_data(&wrapped).pointer("/freshness/active_jobs"),
            Some(&json!(0)),
        );
    }

    fn persisted_registry(runner_id: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(path(runner_id).expect("registry path"))
            .expect("read registry");
        serde_json::from_str(&raw).expect("parse registry")
    }

    #[test]
    fn tombstones_a_large_dead_direct_generation_inventory_without_accepting_new_work() {
        test_support::with_isolated_home(|_| {
            let mut generations =
                RollingGenerations::new("lease-0", session("lease-0", "daemon-0", Some(1000)));
            for index in 1..58 {
                let lease = format!("lease-{index}");
                generations.begin(
                    lease.clone(),
                    session(&lease, &format!("daemon-{index}"), Some(1000 + index)),
                );
            }
            write("runner-a", &generations).expect("persist stale inventory");

            let leases = (0..58)
                .map(|index| format!("lease-{index}"))
                .collect::<Vec<_>>();
            tombstone_dead_direct_generations("runner-a", &leases)
                .expect("dead authoritative daemon tombstones stale inventory");

            assert!(read("runner-a", None)
                .expect("read tombstoned registry")
                .is_none());
        });
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

            fn terminate_tunnel(&self, _: &RunnerSession) {}
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
    fn dropped_generation_a_reconnect_retains_one_owner_after_admission_rotates_to_b() {
        test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            record_job("runner-a", &stale, "job-stale").expect("record stale job");
            activate(
                "runner-a",
                &stale,
                "lease-fresh".to_string(),
                fresh.clone(),
                &["job-stale".to_string()],
            )
            .expect("activate fresh generation");

            let mut reattached = stale.clone();
            reattached.local_url = Some("http://127.0.0.1:49152".to_string());
            reattached.local_port = Some(49152);
            reattached.tunnel_pid = Some(303);
            let replaced = record_reconnected_job_owner("runner-a", &reattached, "job-stale")
                .expect("retain reattached owner");
            assert_eq!(
                replaced, stale,
                "the dropped generation-A endpoint is superseded"
            );

            assert_eq!(
                job_session("runner-a", "job-stale", Some(&fresh)).expect("route stale job"),
                Some(reattached)
            );
            assert_eq!(
                admission_session("runner-a", Some(&stale)).expect("retain admission owner"),
                Some(fresh)
            );
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
            // The durable owner ledger retains the original job alongside the
            // current generation job. Consumers use these identities, rather
            // than the overlapping per-generation counters, to deduplicate
            // admission pressure after rotation.
            let owners = status_job_owners("runner-a", Some(&b)).expect("owner projection");
            assert_eq!(
                owners,
                vec![
                    RunnerGenerationJobOwners {
                        generation: "build-b".to_string(),
                        job_ids: vec!["job-b".to_string()]
                    },
                    RunnerGenerationJobOwners {
                        generation: "lease-a".to_string(),
                        job_ids: vec!["job-a".to_string()]
                    },
                ]
            );
            let report = RunnerStatusReport {
                runner_id: "runner-a".to_string(),
                connected: true,
                state: RunnerSessionState::Connected,
                session: Some(b.clone()),
                stale_daemon: None,
                configured_job_binary_build_identity: None,
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
                json!({
                    "admission_owner": "build-b",
                    "draining": 1,
                    "total": 2,
                })
            );

            let report = RunnerStatusReport {
                runner_id: "runner-a".to_string(),
                connected: true,
                state: RunnerSessionState::Connected,
                session: Some(b.clone()),
                stale_daemon: None,
                configured_job_binary_build_identity: None,
                daemon_freshness: None,
                active_jobs: vec![homeboy_core::api_jobs::ActiveRunnerJobSummary {
                    runner_id: "runner-a".to_string(),
                    job_id: "job-b".to_string(),
                    operation: "runner.exec".to_string(),
                    source: "daemon".to_string(),
                    kind: "runner.exec".to_string(),
                    status: homeboy_core::api_jobs::JobStatus::Running,
                    command: "true".to_string(),
                    cwd: None,
                    started_at_ms: 0,
                    updated_at_ms: 0,
                    elapsed_ms: 0,
                    heartbeat_age_ms: 0,
                    claim: homeboy_core::api_jobs::JobClaimMetadata::default(),
                    claim_expires_in_ms: None,
                    lifecycle: None,
                    durable_run_id: None,
                    stale_reason: None,
                    lifecycle_state: None,
                    retryable: None,
                    active_child_count: None,
                    active_cell_count: None,
                }],
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
            let summary = report.admission_summary_with_generations(&projected, &owners, 1);
            assert_eq!(summary.live_daemon_job_count, 1);
            assert_eq!(summary.retained_durable_job_count, 2);
            assert_eq!(summary.active_job_count, 1);
            assert_eq!(summary.unresolved_retained_projection_count, 1);
            assert!(summary.admission_blocking_job_ids.is_empty());
            assert_eq!(summary.unresolved_generation_ids, ["lease-a"]);

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
            let first =
                reconcile_with("runner-a", Some(&c), &operations).expect("reconcile terminal A");
            assert_eq!(first.retired_generation_ids, ["lease-a"]);

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
            let second =
                reconcile_with("runner-a", Some(&c), &operations).expect("reconcile terminal B");
            assert_eq!(second.retired_generation_ids, ["build-b"]);
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
