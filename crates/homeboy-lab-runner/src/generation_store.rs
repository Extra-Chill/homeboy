use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;
use homeboy_core::server::SshClient;

use crate::runner_state_lock::{with_runner_registry_lock, write_durable_json};
use crate::{
    RollingGenerations, RunnerDaemonGenerationStatus, RunnerGenerationJobOwners, RunnerSession,
};

/// The durable generation registry is scoped to one runner. Keep this wrapper
/// at the state-store boundary so rolling ownership remains reusable in memory.
#[derive(serde::Serialize, serde::Deserialize)]
struct GenerationRegistry<E> {
    runner_id: String,
    #[serde(flatten)]
    generations: RollingGenerations<E>,
}

/// The registry file below an already-resolved config root.
///
/// Every path in this module hangs off one runner-sessions directory. The
/// fallible ambient forms below each read the environment again, so a lock
/// taken through one and a write performed through another are not guaranteed
/// to name the same installation (#7505).
fn path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    paths::runner_sessions_dir_in_root(config_root)
        .join(runner_id)
        .join("generations.json")
}

fn path(runner_id: &str) -> Result<PathBuf> {
    Ok(path_in_root(&paths::homeboy()?, runner_id))
}

/// Lab-runner session discovery treats every JSON child as a session record,
/// so retirement diagnostics live in a non-JSON sidecar.
fn retirement_verdicts_path(runner_id: &str) -> Result<PathBuf> {
    Ok(path(runner_id)?.with_file_name("retirement-verdicts.state"))
}

/// Last retirement decision, owned by reconcile rather than a status probe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct GenerationRetirementVerdict {
    pub checked_at: String,
    #[serde(flatten)]
    pub verdict: GenerationRetirementDecision,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GenerationRetirementDecision {
    Ready,
    Blocked {
        kind: GenerationRetirementBlockerKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenerationRetirementBlockerKind {
    Ownership,
    ActiveWork,
    EndpointUnavailable,
    RetainedEvidence,
    StopUnconfirmed,
}

fn persist_retirement_verdicts(
    runner_id: &str,
    generations: &RollingGenerations<RunnerSession>,
    blockers: &std::collections::BTreeMap<String, String>,
    evidence_blockers: &std::collections::BTreeSet<String>,
    evidence_runs: &std::collections::BTreeMap<String, String>,
    ownership_blockers: &std::collections::BTreeSet<String>,
    retired: &[String],
) -> Result<()> {
    let checked_at = Utc::now().to_rfc3339();
    let verdicts = generations
        .generations
        .iter()
        .filter(|(id, entry)| {
            *id != &generations.admission_owner
                && entry.drain_state == crate::RollingDrainState::Draining
        })
        .map(|(id, entry)| {
            let reason = blockers.get(id).cloned();
            let verdict = match reason {
                None => GenerationRetirementDecision::Ready,
                Some(reason) => {
                    let kind = if evidence_blockers.contains(id) {
                        GenerationRetirementBlockerKind::RetainedEvidence
                    } else if ownership_blockers.contains(id) {
                        GenerationRetirementBlockerKind::Ownership
                    } else if entry.observed_active_jobs.is_some_and(|count| count > 0) {
                        GenerationRetirementBlockerKind::ActiveWork
                    } else if entry.observed_active_jobs.is_none() {
                        GenerationRetirementBlockerKind::EndpointUnavailable
                    } else {
                        GenerationRetirementBlockerKind::StopUnconfirmed
                    };
                    GenerationRetirementDecision::Blocked {
                        kind,
                        run_id: evidence_runs.get(id).cloned(),
                        reason,
                    }
                }
            };
            (
                id.clone(),
                GenerationRetirementVerdict {
                    checked_at: checked_at.clone(),
                    verdict,
                },
            )
        })
        .chain(retired.iter().map(|id| {
            (
                id.clone(),
                GenerationRetirementVerdict {
                    checked_at: checked_at.clone(),
                    verdict: GenerationRetirementDecision::Ready,
                },
            )
        }))
        .collect::<std::collections::BTreeMap<_, _>>();
    write_durable_json(&retirement_verdicts_path(runner_id)?, &verdicts)
}

pub fn persisted_retirement_verdicts(
    runner_id: &str,
) -> Result<std::collections::BTreeMap<String, GenerationRetirementVerdict>> {
    let path = retirement_verdicts_path(runner_id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error.into()),
    };
    Ok(serde_json::from_slice(&bytes)?)
}

/// The generation key a session is registered under: its daemon lease.
#[cfg(test)]
fn session_generation(session: &RunnerSession) -> String {
    session
        .remote_daemon_lease_id
        .clone()
        .unwrap_or_else(|| "unleased".to_string())
}

#[cfg(test)]
const AUTHENTICATED_ADMISSION_SYNC_DIR_ENV: &str = "HOMEBOY_AUTHENTICATED_ADMISSION_SYNC_DIR";

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

/// Read this runner's generation registry. `None` means no registry has been
/// written yet; a registry that does not parse as the current shape is an
/// error rather than something to repair.
pub(crate) fn read(runner_id: &str) -> Result<Option<RollingGenerations<RunnerSession>>> {
    let path = path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    validate_registry_shape(&value)?;
    let registry = serde_json::from_value::<GenerationRegistry<RunnerSession>>(value)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if registry.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "runner_id",
            Some(registry.runner_id),
            format!("generation registry must match runner-scoped path `{runner_id}`"),
        ));
    }
    Ok(Some(registry.generations))
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
                | "retired_evidence"
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

pub(crate) fn admission_session(runner_id: &str) -> Result<Option<RunnerSession>> {
    Ok(read(runner_id)?.and_then(|generations| {
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
    with_runner_registry_lock(runner_id, || {
        // A healthy, leased direct session is registry authority on its own:
        // seed the registry from it when none exists yet.
        let mut generations =
            read(runner_id)?.unwrap_or_else(|| RollingGenerations::new(lease_id, session.clone()));
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
        generations.activate_preserving_drained(&generation);
        generations
            .generations
            .get_mut(&generation)
            .expect("generation was inserted or found")
            .endpoint = session.clone();
        write(runner_id, &generations)
    })
}

pub(crate) fn status_projection(runner_id: &str) -> Result<Vec<RunnerDaemonGenerationStatus>> {
    Ok(status_admission_projection(runner_id)?.0)
}

/// Return durable job identities grouped by generation. These identities can
/// be deduplicated with live daemon jobs; persisted counters cannot.
pub(crate) fn status_job_owners(runner_id: &str) -> Result<Vec<RunnerGenerationJobOwners>> {
    Ok(status_admission_projection(runner_id)?.1)
}

/// Capture the complete admission ledger from one persisted generation read.
/// Inventory and durable job ownership must describe the same generation state.
pub(crate) fn status_admission_projection(
    runner_id: &str,
) -> Result<(
    Vec<RunnerDaemonGenerationStatus>,
    Vec<RunnerGenerationJobOwners>,
)> {
    Ok(read(runner_id)?.map_or_else(
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
                    active_job_count: job_owner_ids_for(&generations, generation, &entry.endpoint)
                        .len(),
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

pub(crate) fn clear(runner_id: &str) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
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
    with_runner_registry_lock(runner_id, || {
        let Some(generations) = read(runner_id)? else {
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
    fn queued_jobs(&self, _session: &RunnerSession) -> Option<Vec<crate::RunnerJob>> {
        None
    }
    fn cancel_job(
        &self,
        _session: &RunnerSession,
        _job_id: &str,
        _request: &homeboy_core::api_jobs::RunnerJobCancelRequest,
    ) -> bool {
        false
    }

    /// Returns one inventory result per path, in request order.
    fn evidence(
        &self,
        _session: &RunnerSession,
        paths: &[String],
    ) -> Vec<Option<serde_json::Value>> {
        vec![None; paths.len()]
    }

    /// The endpoint's own record for one job it owns, used to release ledger
    /// ownership for work that already finished. It is read while the
    /// generation still reports active jobs, because a stale owner is exactly
    /// why that count has not reached zero.
    fn job_record(&self, _session: &RunnerSession, _job_id: &str) -> Option<serde_json::Value> {
        None
    }

    /// Terminal linked handoffs can survive a controller restart in a daemon's
    /// job store. Settle them before treating its active count as ownership.
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool;
    fn active_jobs(&self, session: &RunnerSession) -> Option<usize>;
    fn proven_stopped(&self, session: &RunnerSession) -> bool;
    fn stop(&self, session: &RunnerSession) -> bool;
    fn terminate_tunnel(&self, session: &RunnerSession);
}

fn queued_generation_jobs(
    response: &serde_json::Value,
    session: &RunnerSession,
) -> Option<Vec<crate::RunnerJob>> {
    let body = response.pointer("/data/body").unwrap_or(response);
    let jobs: Vec<homeboy_core::api_jobs::ActiveRunnerJobSummary> =
        serde_json::from_value(body.get("active_runner_jobs")?.clone()).ok()?;
    Some(
        jobs.iter()
            .filter(|job| {
                job.runner_id == session.runner_id
                    && job.status == homeboy_core::api_jobs::JobStatus::Queued
            })
            .map(crate::RunnerJob::from)
            .collect(),
    )
}

/// Replay an existing controller cancellation to an unstarted handoff on its
/// authenticated owning generation. A timeout or stale heartbeat is not proof
/// of cancellation; absent, active and differently bound runs remain untouched.
fn reconcile_cancelled_queued_jobs(
    session: &RunnerSession,
    operations: &impl GenerationEndpointOperations,
    is_cancelled: impl Fn(&str) -> bool,
) {
    for job in operations.queued_jobs(session).unwrap_or_default() {
        let Some(run_id) = job.durable_run_id.as_deref() else {
            continue;
        };
        if job.status == homeboy_core::api_jobs::JobStatus::Queued
            && job.runner_id == session.runner_id
            && job
                .lifecycle
                .as_ref()
                .and_then(|lifecycle| lifecycle.durable_run_id.as_deref())
                == Some(run_id)
            && is_cancelled(run_id)
        {
            operations.cancel_job(
                session,
                &job.job_id,
                &homeboy_core::api_jobs::RunnerJobCancelRequest::from_controller(&format!(
                    "replay controller cancellation of queued run {run_id}"
                )),
            );
        }
    }
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
    fn queued_jobs(&self, session: &RunnerSession) -> Option<Vec<crate::RunnerJob>> {
        if self.active_jobs(session)? == 0 {
            return Some(Vec::new());
        }
        let response = self
            .client
            .get(format!(
                "{}/jobs",
                session.local_url.as_deref()?.trim_end_matches('/')
            ))
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .ok()?;
        queued_generation_jobs(&response, session)
    }

    fn cancel_job(
        &self,
        session: &RunnerSession,
        job_id: &str,
        request: &homeboy_core::api_jobs::RunnerJobCancelRequest,
    ) -> bool {
        if self.active_jobs(session).is_none() {
            return false;
        }
        let Some(url) = session.local_url.as_deref() else {
            return false;
        };
        let encoded = homeboy_core::execution_contract::encode_uri_component(job_id);
        self.client
            .post(format!(
                "{}/jobs/{encoded}/cancel",
                url.trim_end_matches('/')
            ))
            .json(request)
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    fn job_record(&self, session: &RunnerSession, job_id: &str) -> Option<serde_json::Value> {
        let encoded = homeboy_core::execution_contract::encode_uri_component(job_id);
        self.client
            .get(format!(
                "{}/jobs/{encoded}",
                session.local_url.as_deref()?.trim_end_matches('/')
            ))
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .ok()
    }

    fn evidence(
        &self,
        session: &RunnerSession,
        paths: &[String],
    ) -> Vec<Option<serde_json::Value>> {
        paths
            .iter()
            .map(|path| {
                self.client
                    .get(format!(
                        "{}{path}",
                        session.local_url.as_deref()?.trim_end_matches('/')
                    ))
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json()
                    .ok()
            })
            .collect()
    }
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
        if self.active_jobs(session).is_none() {
            return false;
        }
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
        if !SshGenerationEndpointOperations::health_matches_session(
            session,
            daemon_health_data(&health),
        ) {
            return None;
        }
        daemon_health_data(&health)
            .pointer("/freshness/active_jobs")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
    }

    fn proven_stopped(&self, _session: &RunnerSession) -> bool {
        false
    }

    fn stop(&self, session: &RunnerSession) -> bool {
        if self.active_jobs(session) != Some(0) {
            return false;
        }
        let (Some(local_url), Some(lease_id)) = (
            session.local_url.as_deref(),
            session.remote_daemon_lease_id.as_deref(),
        ) else {
            return false;
        };
        let accepted = self
            .client
            .post(format!(
                "{}/lifecycle/stop",
                local_url.trim_end_matches('/')
            ))
            .json(&serde_json::json!({ "lease_id": lease_id, "force": false }))
            .send()
            .is_ok_and(|response| response.status().is_success());
        if !accepted {
            return false;
        }
        // The HTTP response acknowledges a request before the daemon's final
        // token/idle gate. A still-serving endpoint is not a retired process.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self
                .client
                .get(format!("{}/health", local_url.trim_end_matches('/')))
                .timeout(Duration::from_millis(500))
                .send()
                .is_err()
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
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

impl GenerationEndpointOperations for SshGenerationEndpointOperations<'_> {
    fn queued_jobs(&self, session: &RunnerSession) -> Option<Vec<crate::RunnerJob>> {
        if self.active_jobs(session)? == 0 {
            return Some(Vec::new());
        }
        let response = serde_json::from_str(&self.request("GET", session, "/jobs", None)?).ok()?;
        queued_generation_jobs(&response, session)
    }

    fn cancel_job(
        &self,
        session: &RunnerSession,
        job_id: &str,
        request: &homeboy_core::api_jobs::RunnerJobCancelRequest,
    ) -> bool {
        if self.authenticated_health(session).is_none() {
            return false;
        }
        let encoded = homeboy_core::execution_contract::encode_uri_component(job_id);
        let Ok(body) = serde_json::to_string(request) else {
            return false;
        };
        self.request(
            "POST",
            session,
            &format!("/jobs/{encoded}/cancel"),
            Some(&body),
        )
        .is_some()
    }

    fn job_record(&self, session: &RunnerSession, job_id: &str) -> Option<serde_json::Value> {
        let encoded = homeboy_core::execution_contract::encode_uri_component(job_id);
        serde_json::from_str(&self.request("GET", session, &format!("/jobs/{encoded}"), None)?).ok()
    }

    fn evidence(
        &self,
        session: &RunnerSession,
        paths: &[String],
    ) -> Vec<Option<serde_json::Value>> {
        if paths.is_empty() {
            return Vec::new();
        }
        let mut command = String::new();
        for path in paths {
            let Some(url) = Self::endpoint_url(session, path) else {
                return vec![None; paths.len()];
            };
            // A raw record separator cannot occur in a valid JSON body. Keep
            // failed curl output out of its frame, but always emit the separator.
            command.push_str(&format!(
                "body=$(curl --fail --silent --show-error --max-time 5 {} ) && printf '%s' \"$body\"; printf '\\036'; ",
                shell::quote_arg(&url),
            ));
        }
        let timeout = Duration::from_secs(5 * paths.len() as u64 + 5);
        let output = self.client.execute_with_timeout(&command, timeout);
        if !output.success {
            return vec![None; paths.len()];
        }
        let mut bodies = output.stdout.split_terminator('\u{001e}');
        paths
            .iter()
            .map(|_| {
                bodies
                    .next()
                    .and_then(|body| serde_json::from_str(body).ok())
            })
            .collect()
    }
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

    fn proven_stopped(&self, session: &RunnerSession) -> bool {
        let Some(pid) = session.remote_daemon_pid else {
            return false;
        };
        self.client
            .execute_with_timeout(
                &format!("kill -0 {pid} 2>/dev/null"),
                Duration::from_secs(5),
            )
            .exit_code
            == 1
    }

    fn stop(&self, session: &RunnerSession) -> bool {
        let Some(lease_id) = session.remote_daemon_lease_id.as_deref() else {
            return false;
        };
        if self.authenticated_health(session).is_none() {
            return false;
        }
        let body = serde_json::json!({ "lease_id": lease_id, "force": false }).to_string();
        if self
            .request("POST", session, "/lifecycle/stop", Some(&body))
            .is_none()
        {
            return false;
        }
        self.proven_stopped(session)
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
    fn queued_jobs(&self, session: &RunnerSession) -> Option<Vec<crate::RunnerJob>> {
        self.primary
            .queued_jobs(session)
            .or_else(|| self.fallback.queued_jobs(session))
    }

    fn cancel_job(
        &self,
        session: &RunnerSession,
        job_id: &str,
        request: &homeboy_core::api_jobs::RunnerJobCancelRequest,
    ) -> bool {
        self.primary.cancel_job(session, job_id, request)
            || self.fallback.cancel_job(session, job_id, request)
    }

    fn evidence(
        &self,
        session: &RunnerSession,
        paths: &[String],
    ) -> Vec<Option<serde_json::Value>> {
        let mut results = self.primary.evidence(session, paths);
        fill_missing_evidence(&mut results, paths, |missing| {
            self.fallback.evidence(session, missing)
        });
        results
    }

    fn job_record(&self, session: &RunnerSession, job_id: &str) -> Option<serde_json::Value> {
        self.primary
            .job_record(session, job_id)
            .or_else(|| self.fallback.job_record(session, job_id))
    }
    fn reconcile_terminal_jobs(&self, session: &RunnerSession) -> bool {
        self.primary.reconcile_terminal_jobs(session)
            || self.fallback.reconcile_terminal_jobs(session)
    }

    fn active_jobs(&self, session: &RunnerSession) -> Option<usize> {
        self.primary
            .active_jobs(session)
            .or_else(|| self.fallback.active_jobs(session))
    }

    fn proven_stopped(&self, session: &RunnerSession) -> bool {
        self.primary.proven_stopped(session) || self.fallback.proven_stopped(session)
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
    pub retirement_blockers: std::collections::BTreeMap<String, String>,
    pub retained_evidence_generation_count: usize,
    /// Idle draining generations whose only blocker is retained-evidence
    /// verification (#15653). `--release-unverified-evidence` can retire them.
    pub evidence_blocked_generation_ids: std::collections::BTreeSet<String>,
    /// Generations retired under `--release-unverified-evidence`, with the
    /// exact verification failure each was released despite. Their run and
    /// artifact ownership stays recorded as retired evidence; nothing is
    /// deleted on the controller or the runner.
    pub released_unverified_evidence: std::collections::BTreeMap<String, String>,
    /// Proven-stopped draining generations whose job owners could only be
    /// released by operator attestation (#15779), with those job IDs.
    pub stopped_generation_job_owner_ids: std::collections::BTreeMap<String, Vec<String>>,
    /// Job owners released under `release_stopped_generation_jobs`, by the
    /// generation that retired. The generation endpoint is kept as retired
    /// evidence; nothing is deleted on the controller or the runner.
    pub released_stopped_generation_jobs: std::collections::BTreeMap<String, Vec<String>>,
}

/// Operator choices for one generation reconcile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GenerationReconcilePolicy {
    /// Retire idle draining generations whose retained evidence cannot be
    /// verified (legacy custody, unreachable source inventory). Explicit only:
    /// the default stays fail-closed (#15653).
    pub release_unverified_evidence: bool,
    /// The operator attests that no workload process survives a draining
    /// generation whose daemon is proven stopped. Its remaining job owners
    /// can never be observed terminal (only the dead daemon could answer),
    /// so they are released as retired evidence instead of pinning the
    /// generation forever (#15779). Explicit only; the default stays
    /// fail-closed.
    pub release_stopped_generation_jobs: bool,
}

fn verify_controller_copy(
    source: &homeboy_core::observation::ArtifactRecord,
    local: &[homeboy_core::observation::ArtifactRecord],
) -> Result<()> {
    let copied = local.iter().find(|artifact| artifact.id == source.id)
        .and_then(|artifact| {
            if artifact.artifact_type == "remote_file" {
                local.iter().find(|mirror| {
                    mirror.artifact_type == "file"
                        && mirror.metadata_json["agent_task"]["projection"] == "runner_mirrored"
                        && mirror.sha256 == artifact.sha256
                        && mirror.size_bytes == artifact.size_bytes
                })
            } else {
                Some(artifact)
            }
        })
        .filter(|copy| copy.sha256 == source.sha256 && copy.size_bytes == source.size_bytes)
        .ok_or_else(|| Error::internal_unexpected(format!(
            "retained evidence {}: controller copy or matching runner_mirrored file is missing or differs from source", source.id
        )))?;
    crate::evidence::validate_controller_artifact_bytes(copied)
}

/// Result references are provenance, not a reason to keep a process alive.
/// Deleted controller runs have released their evidence obligation. Retained
/// runs must be terminal with verified controller-owned bytes and complete
/// source inventories. Unknown/legacy references remain fail-closed.
/// Fill each unread entry of `results` from `fallback`, which receives only
/// the unread paths, in request order.
fn fill_missing_evidence(
    results: &mut [Option<serde_json::Value>],
    paths: &[String],
    fallback: impl FnOnce(&[String]) -> Vec<Option<serde_json::Value>>,
) {
    let missing: Vec<String> = paths
        .iter()
        .zip(results.iter())
        .filter(|(_, result)| result.is_none())
        .map(|(path, _)| path.clone())
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut filled = fallback(&missing).into_iter();
    for result in results.iter_mut().filter(|result| result.is_none()) {
        *result = filled.next().flatten();
    }
}

fn verify_retained_evidence(
    runner_id: &str,
    generations: &RollingGenerations<RunnerSession>,
    generation: &str,
    endpoint: &RunnerSession,
    operations: &impl GenerationEndpointOperations,
) -> Result<Option<&'static str>> {
    use homeboy_core::observation::ObservationStore;
    let runs = generations
        .run_owners
        .iter()
        .filter(|(_, owner)| owner_matches_generation(owner, generation, endpoint))
        .map(|(id, _)| id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let artifacts = generations
        .artifact_owners
        .iter()
        .filter(|(_, owner)| owner_matches_generation(owner, generation, endpoint))
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    if runs.is_empty() && artifacts.is_empty() {
        return Ok(None);
    }
    // Asked only when a source inventory is unavailable: proof costs a remote
    // probe, and a reachable inventory must still be compared.
    let stopped = std::cell::OnceCell::new();
    let proven_stopped = || {
        *stopped.get_or_init(|| {
            generations
                .generations
                .get(generation)
                .is_some_and(|entry| {
                    entry.drain_state == crate::RollingDrainState::Draining
                        && operations.proven_stopped(endpoint)
                })
        })
    };
    let mut discharged = false;
    let store = ObservationStore::open_initialized()?;
    let invalid = |id: &str, reason: &str| {
        let mut error = Error::internal_unexpected(format!("retained evidence {id}: {reason}"));
        let run_id = if runs.contains(id) {
            Some(id.to_string())
        } else {
            store
                .get_artifact(id)
                .ok()
                .flatten()
                .map(|artifact| artifact.run_id)
        };
        error.details = serde_json::json!({"run_id": run_id});
        error
    };
    // The admitting endpoint may still serve shared run evidence when the
    // owning endpoint is unreachable but has not been proven stopped.
    let admitting = generations
        .generations
        .get(&generations.admission_owner)
        .map(|entry| &entry.endpoint);
    let mut retained_runs = Vec::new();
    let mut released_runs = std::collections::BTreeSet::new();
    for id in &runs {
        // Controller retention has already released this run's evidence.
        // Absence is authoritative; storage errors above still fail closed.
        let Some(run) = store.get_run(id)? else {
            released_runs.insert(*id);
            continue;
        };
        let owner = run
            .metadata_json
            .pointer("/lab/runner/id")
            .or_else(|| run.metadata_json.pointer("/lab_offload/runner_id"))
            .or_else(|| run.metadata_json.pointer("/runner_id"))
            .and_then(serde_json::Value::as_str);
        // run_owners is authoritative; recovery-only agent-task metadata may
        // have no runner identity. Reject an explicit conflicting identity.
        if owner.is_some_and(|owner| owner != runner_id) {
            return Err(invalid(
                id,
                "controller run runner identity is inconsistent",
            ));
        }
        if !homeboy_core::observation::RunStatus::from_label(&run.status)
            .is_some_and(|status| status.is_terminal() && !status.defers_outcome_to_a_remote_run())
        {
            return Err(invalid(id, "controller run is not terminal"));
        }
        retained_runs.push(*id);
    }
    let paths: Vec<_> = retained_runs
        .iter()
        .flat_map(|id| {
            let encoded = homeboy_core::execution_contract::encode_uri_component(id);
            [
                format!("/runs/{encoded}/artifacts"),
                format!("/runs/{encoded}/findings"),
            ]
        })
        .collect();
    let mut evidence = if paths.is_empty() {
        Vec::new()
    } else {
        operations.evidence(endpoint, &paths)
    };
    if let Some(admitting) = admitting.filter(|admitting| *admitting != endpoint) {
        fill_missing_evidence(&mut evidence, &paths, |missing| {
            operations.evidence(admitting, missing)
        });
    }
    for (id, inventories) in retained_runs.iter().zip(evidence.chunks_exact(2)) {
        let local_artifacts = store.list_artifacts(id)?;
        // A valid local file does not prove the whole remote result was copied.
        // Compare exhaustive source inventories before releasing its daemon.
        let Some(remote) = &inventories[0] else {
            if !proven_stopped() {
                return Err(invalid(id, "source artifact inventory unavailable"));
            }
            // Only the stopped daemon held the inventory; every controller
            // copy it left must still be valid.
            for artifact in &local_artifacts {
                verify_controller_copy(artifact, &local_artifacts)
                    .map_err(|error| invalid(id, &error.message))?;
            }
            discharged = true;
            continue;
        };
        let remote_artifacts = remote
            .pointer("/data/body/artifacts")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid(id, "source artifact inventory invalid"))?;
        for source in remote_artifacts {
            let source: homeboy_core::observation::ArtifactRecord =
                serde_json::from_value(source.clone())
                    .map_err(|_| invalid(id, "source artifact metadata invalid"))?;
            verify_controller_copy(&source, &local_artifacts)
                .map_err(|error| invalid(id, &error.message))?;
        }
        let Some(remote) = &inventories[1] else {
            if !proven_stopped() {
                return Err(invalid(id, "source findings unavailable"));
            }
            discharged = true;
            continue;
        };
        let remote_findings = remote
            .pointer("/data/body/findings")
            .ok_or_else(|| invalid(id, "source findings invalid"))?;
        let local_findings = store.list_findings(homeboy_core::observation::FindingListFilter {
            run_id: Some((*id).to_string()),
            ..Default::default()
        })?;
        if serde_json::to_value(local_findings).ok().as_ref() != Some(remote_findings) {
            return Err(invalid(id, "source findings have not been fully copied"));
        }
    }
    for id in artifacts {
        let Some(artifact) = store.get_artifact(id)? else {
            // Retention may remove the run and its artifacts together. Only
            // discharge an absent artifact when every run obligation was released.
            if !runs.is_empty() && released_runs.len() == runs.len() {
                continue;
            }
            return Err(invalid(id, "controller artifact is missing"));
        };
        if store.get_run(&artifact.run_id)?.is_none() {
            continue;
        }
        if !runs.contains(artifact.run_id.as_str()) {
            return Err(invalid(
                id,
                "artifact has no retained run owned by this generation",
            ));
        }
        verify_controller_copy(&artifact, &store.list_artifacts(&artifact.run_id)?)
            .map_err(|error| invalid(id, &error.message))?;
    }
    Ok(discharged
        .then_some("daemon proven stopped; source inventory and findings checks discharged"))
}

/// Read a daemon job record and report whether it is in a terminal state.
///
/// The status must be present and recognized. An unreachable endpoint, an
/// unparsable record, or an unknown status is never treated as terminal, so a
/// job whose fate is unknown keeps its generation.
fn job_record_is_terminal(record: &serde_json::Value) -> bool {
    record
        .pointer("/data/body/job/status")
        .or_else(|| record.pointer("/body/job/status"))
        .or_else(|| record.pointer("/job/status"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| {
            serde_json::from_value::<homeboy_core::api_jobs::JobStatus>(serde_json::Value::String(
                status.to_string(),
            ))
            .is_ok_and(homeboy_core::api_jobs::JobStatus::is_terminal)
        })
}

/// Ownership consistency measured before any endpoint is stopped.
#[derive(Debug, Default)]
struct OwnershipAudit {
    retirement_blockers: std::collections::BTreeMap<String, String>,
    fences_retirement: bool,
}

/// Classify every persisted owner against the endpoints this registry knows.
///
/// Ambiguous ownership is unsafe to act on, because one identity would resolve
/// to more than one endpoint. A job owner whose generation is known only as
/// retained evidence is unsafe too: that endpoint is the inspection and
/// cancellation authority for work which may still exist. Both fence
/// retirement for the whole runner.
///
/// An owner that resolves to nothing is already unroutable and names a
/// generation this registry no longer knows. Retiring an unrelated,
/// verified-idle generation cannot make that record worse, so it is reported as
/// a diagnostic rather than fencing every other generation behind it.
fn audit_ownership(generations: &RollingGenerations<RunnerSession>) -> OwnershipAudit {
    let mut audit = OwnershipAudit::default();
    for (id, owner) in generations
        .job_owners
        .iter()
        .chain(generations.run_owners.iter())
        .chain(generations.artifact_owners.iter())
    {
        let live = generations
            .generations
            .iter()
            .filter(|(generation, entry)| {
                owner_matches_generation(owner, generation, &entry.endpoint)
            })
            .count();
        let retired = generations
            .retired_evidence
            .iter()
            .filter(|(generation, endpoint)| owner_matches_generation(owner, generation, endpoint))
            .count();
        let owns_job = generations.job_owners.contains_key(id);
        let ambiguous = live + retired > 1;
        let dangling_owner = live + retired == 0;
        let unresolved_job_owner = owns_job && live == 0 && retired >= 1;
        if !(ambiguous || unresolved_job_owner || dangling_owner) {
            continue;
        }
        audit.fences_retirement |= ambiguous || unresolved_job_owner;
        audit.retirement_blockers.insert(
            owner.clone(),
            format!("ownership for {id} resolves to {live} live and {retired} retired generations"),
        );
    }
    audit
}

pub(crate) fn reconcile(runner_id: &str) -> Result<GenerationReconcileResult> {
    reconcile_with_policy(runner_id, GenerationReconcilePolicy::default())
}

pub(crate) fn reconcile_with_policy(
    runner_id: &str,
    policy: GenerationReconcilePolicy,
) -> Result<GenerationReconcileResult> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build generation reconcile client: {error}"))
        })?;
    reconcile_with_operations(
        runner_id,
        &HttpGenerationEndpointOperations { client },
        policy,
    )
}

/// Reconcile generations through their controller-local tunnels, falling back
/// to the same recorded loopback daemon endpoints over the trusted SSH runner.
/// This restores observability after an old generation's local tunnel exits
/// without weakening the daemon's terminal-job, zero-active-job, or stop gates.
pub(crate) fn reconcile_with_ssh_and_policy(
    runner_id: &str,
    ssh_client: &SshClient,
    policy: GenerationReconcilePolicy,
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
    reconcile_with_operations(
        runner_id,
        &FallbackGenerationEndpointOperations {
            primary: &local,
            fallback: &remote,
        },
        policy,
    )
}

#[cfg(test)]
fn reconcile_with(
    runner_id: &str,
    operations: &impl GenerationEndpointOperations,
) -> Result<GenerationReconcileResult> {
    reconcile_with_operations(runner_id, operations, GenerationReconcilePolicy::default())
}

/// The generation's retained run and artifact ownership is unchanged since
/// `prior` was observed, so evidence verified against it still applies.
fn retained_ownership_unchanged(
    current: &RollingGenerations<RunnerSession>,
    prior: &RollingGenerations<RunnerSession>,
    generation: &str,
    session: &RunnerSession,
) -> bool {
    fn owned<'a>(
        owners: &'a std::collections::BTreeMap<String, String>,
        generation: &str,
        session: &RunnerSession,
    ) -> Vec<(&'a String, &'a String)> {
        owners
            .iter()
            .filter(|(_, owner)| owner_matches_generation(owner, generation, session))
            .collect()
    }
    owned(&current.run_owners, generation, session) == owned(&prior.run_owners, generation, session)
        && owned(&current.artifact_owners, generation, session)
            == owned(&prior.artifact_owners, generation, session)
}

fn reconcile_with_operations(
    runner_id: &str,
    operations: &impl GenerationEndpointOperations,
    policy: GenerationReconcilePolicy,
) -> Result<GenerationReconcileResult> {
    let mut evidence_blocked_generation_ids = std::collections::BTreeSet::new();
    let mut evidence_runs = std::collections::BTreeMap::new();
    let mut released_unverified_evidence = std::collections::BTreeMap::new();
    let mut stopped_generation_job_owner_ids = std::collections::BTreeMap::new();
    let mut released_stopped_generation_jobs = std::collections::BTreeMap::new();
    let Some(generations) = read(runner_id)? else {
        return Ok(GenerationReconcileResult::default());
    };
    let OwnershipAudit {
        mut retirement_blockers,
        fences_retirement,
    } = audit_ownership(&generations);
    let mut ownership_blockers = retirement_blockers
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if fences_retirement {
        for (id, entry) in &generations.generations {
            if entry.drain_state == crate::RollingDrainState::Draining {
                retirement_blockers
                    .entry(id.clone())
                    .or_insert_with(|| "generation ownership is ambiguous".to_string());
                ownership_blockers.insert(id.clone());
            }
        }
        persist_retirement_verdicts(
            runner_id,
            &generations,
            &retirement_blockers,
            &evidence_blocked_generation_ids,
            &evidence_runs,
            &ownership_blockers,
            &[],
        )?;
        return Ok(GenerationReconcileResult {
            retired_generation_ids: Vec::new(),
            retirement_blockers,
            retained_evidence_generation_count: generations.retired_evidence.len(),
            ..Default::default()
        });
    }
    // Jobs admitted before this observation started are terminal once the
    // admitting daemon reports zero active jobs.
    let idle_observed_since_ms = Utc::now().timestamp_millis();
    let live_idle = generations
        .generations
        .get(&generations.admission_owner)
        .and_then(|entry| operations.active_jobs(&entry.endpoint))
        == Some(0);
    let observations = generations
        .generations
        .iter()
        .map(|(generation, entry)| {
            (
                generation.clone(),
                entry.endpoint.clone(),
                job_owner_ids_for(&generations, generation, &entry.endpoint),
                entry.drain_state == crate::RollingDrainState::Draining,
            )
        })
        .map(|(generation, session, job_owner_ids, draining)| {
            // A job whose submitting client disappeared keeps its ledger
            // ownership until something observes the finished job. Ask the
            // owning endpoint directly so a completed job cannot pin its
            // generation forever, and settle only what it proves terminal.
            let settled_job_ids = draining
                .then(|| {
                    job_owner_ids
                        .iter()
                        .filter(|job_id| {
                            operations
                                .job_record(&session, job_id)
                                .is_some_and(|record| job_record_is_terminal(&record))
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if draining {
                reconcile_cancelled_queued_jobs(
                    &session,
                    operations,
                    homeboy_core::api_jobs::agent_task_terminal_recovery::durable_run_is_cancelled,
                );
            }
            let terminal_settled = !draining || operations.reconcile_terminal_jobs(&session);
            let raw_observed = operations.active_jobs(&session);
            let unclaimed = draining && raw_observed.is_none() && live_idle;
            let observed_active_jobs = if unclaimed {
                Some(0)
            } else if terminal_settled {
                raw_observed
            } else {
                None
            };
            let proven_stopped =
                draining && observed_active_jobs.is_none() && operations.proven_stopped(&session);
            let evidence = if draining && (observed_active_jobs == Some(0) || proven_stopped) {
                verify_retained_evidence(runner_id, &generations, &generation, &session, operations)
            } else {
                Ok(None)
            };
            (
                generation,
                session,
                job_owner_ids,
                observed_active_jobs,
                proven_stopped,
                unclaimed,
                settled_job_ids,
                evidence,
            )
        })
        .collect::<Vec<_>>();
    // Observe outside the lock; recheck the exact endpoint and ownership before
    // committing an observation. The bounded stop/retirement transaction below
    // fences concurrent registry writers.
    let observed_generations = &generations;
    let stoppable = with_runner_registry_lock(runner_id, || {
        if live_idle {
            crate::admission_fence::release_admitted_jobs_observed_idle_locked(
                runner_id,
                idle_observed_since_ms,
            )?;
        }
        let Some(mut generations) = read(runner_id)? else {
            return Ok((Vec::new(), Vec::new()));
        };
        let mut authoritative_zero = Vec::new();
        let mut already_stopped = Vec::new();
        for (
            generation,
            prior_session,
            prior_job_owner_ids,
            observed_active_jobs,
            proven_stopped,
            unclaimed,
            settled_job_ids,
            evidence,
        ) in &observations
        {
            if let Some(entry) = generations.generations.get(generation) {
                let state_unchanged = entry.endpoint == *prior_session
                    && job_owner_ids_for(&generations, generation, &entry.endpoint)
                        == *prior_job_owner_ids
                    && retained_ownership_unchanged(
                        &generations,
                        observed_generations,
                        generation,
                        prior_session,
                    );
                if !state_unchanged {
                    ownership_blockers.insert(generation.clone());
                    retirement_blockers.insert(
                        generation.clone(),
                        "ownership changed during observation".to_string(),
                    );
                    generations
                        .generations
                        .get_mut(generation)
                        .expect("generation was checked")
                        .observed_active_jobs = None;
                    continue;
                }
            }
            // Release ownership the endpoint proved finished, under the same
            // lock that guards retirement. This only removes owners for jobs
            // that were observed terminal on their own generation's endpoint.
            for job_id in settled_job_ids {
                generations.complete_job_preserving_drained(job_id);
            }
            let evidence_error = evidence.as_ref().err();
            // Only evidence verification stands between this idle generation
            // and retirement. Record it, and release it when the operator
            // explicitly asked (#15653); otherwise it stays a blocker.
            let evidence_error = match evidence_error {
                Some(error) => {
                    evidence_blocked_generation_ids.insert(generation.clone());
                    if let Some(run_id) = error
                        .details
                        .get("run_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        evidence_runs.insert(generation.clone(), run_id.to_string());
                    }
                    if policy.release_unverified_evidence {
                        released_unverified_evidence
                            .insert(generation.clone(), error.message.clone());
                        None
                    } else {
                        Some(error)
                    }
                }
                None => None,
            };
            if let Some(entry) = generations.generations.get_mut(generation) {
                entry.observed_active_jobs = *observed_active_jobs;
                if observed_active_jobs == &Some(0) {
                    generations.job_owners.retain(|_, owner| {
                        !owner_matches_generation(owner, generation, &entry.endpoint)
                    });
                }
                if entry.drain_state == crate::RollingDrainState::Draining
                    && observed_active_jobs == &Some(0)
                {
                    if evidence_error.is_none() {
                        if *unclaimed {
                            already_stopped.push(generation.clone());
                        } else {
                            authoritative_zero.push(generation.clone());
                        }
                    }
                }
                // A proven-stopped draining generation cannot be observed, so
                // `observed_active_jobs` stays `None`. With no durable job
                // owner it has nothing left to drain and retires as already
                // stopped; the owner set, not a stored counter, is the proof.
                //
                // Its remaining job owners can only be settled by the stopped
                // daemon itself, so they pin it forever (#15779). Report them,
                // and release them only on the operator's workload-absence
                // attestation.
                let mut releasable_owner_ids = Vec::new();
                if *proven_stopped
                    && !*unclaimed
                    && entry.drain_state == crate::RollingDrainState::Draining
                    && !prior_job_owner_ids.is_empty()
                {
                    stopped_generation_job_owner_ids
                        .insert(generation.clone(), prior_job_owner_ids.clone());
                    if policy.release_stopped_generation_jobs && evidence_error.is_none() {
                        releasable_owner_ids = prior_job_owner_ids.clone();
                    }
                }
                if *proven_stopped
                    && !*unclaimed
                    && entry.drain_state == crate::RollingDrainState::Draining
                    && (prior_job_owner_ids.is_empty() || !releasable_owner_ids.is_empty())
                    && evidence_error.is_none()
                {
                    already_stopped.push(generation.clone());
                    if !releasable_owner_ids.is_empty() {
                        released_stopped_generation_jobs
                            .insert(generation.clone(), releasable_owner_ids);
                    }
                }
                if entry.drain_state == crate::RollingDrainState::Draining {
                    let reason = if let Some(error) = evidence_error {
                        Some(error.to_string())
                    } else if observed_active_jobs.is_some_and(|count| count > 0) {
                        Some(format!("{} active jobs", observed_active_jobs.unwrap()))
                    } else if observed_active_jobs.is_none()
                        && !already_stopped.contains(generation)
                        && stopped_generation_job_owner_ids.contains_key(generation)
                    {
                        Some(format!(
                            "daemon proven stopped; {} job owner(s) need workload-absence attestation",
                            prior_job_owner_ids.len()
                        ))
                    } else if observed_active_jobs.is_none()
                        && !already_stopped.contains(generation)
                    {
                        Some(
                            "endpoint identity/active work unavailable; stop is not authorized"
                                .to_string(),
                        )
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        retirement_blockers.insert(generation.clone(), reason);
                    }
                }
            }
        }
        let stoppable = authoritative_zero
            .iter()
            .filter_map(|generation| {
                generations.generations.get(generation).and_then(|entry| {
                    (entry.drain_state == crate::RollingDrainState::Draining
                        && entry.observed_active_jobs == Some(0)
                        && job_owner_ids_for(&generations, generation, &entry.endpoint).is_empty())
                    .then_some((generation.clone(), entry.endpoint.clone()))
                })
            })
            .collect::<Vec<_>>();
        write(runner_id, &generations)?;
        Ok((stoppable, already_stopped))
    })?;

    let (stoppable, already_stopped) = stoppable;
    let mut candidates = stoppable;
    candidates.extend(already_stopped.iter().filter_map(|generation| {
        generations
            .generations
            .get(generation)
            .map(|entry| (generation.clone(), entry.endpoint.clone()))
    }));
    let retired_generation_ids = with_runner_registry_lock(runner_id, || {
        let Some(mut generations) = read(runner_id)? else {
            return Ok(Vec::new());
        };
        let mut retired = Vec::new();
        for (generation, session) in &candidates {
            let should_remove = generations
                .generations
                .get(generation)
                .is_some_and(|entry| {
                    // A proven-stopped generation cannot be observed, so its
                    // proof of stop stands in for an observed zero; both
                    // still require an empty durable owner set.
                    let owners = job_owner_ids_for(&generations, generation, &entry.endpoint);
                    entry.drain_state == crate::RollingDrainState::Draining
                        && entry.endpoint == *session
                        && (entry.observed_active_jobs == Some(0)
                            || already_stopped.contains(generation))
                        && (owners.is_empty()
                            || released_stopped_generation_jobs.get(generation) == Some(&owners))
                });
            if should_remove {
                let (_, _, _, _, _, _, _, evidence) = observations
                    .iter()
                    .find(|(id, ..)| id == generation)
                    .expect("observed candidate");
                let ownership_unchanged = retained_ownership_unchanged(
                    &generations,
                    observed_generations,
                    generation,
                    session,
                );
                if !ownership_unchanged {
                    ownership_blockers.insert(generation.clone());
                    retirement_blockers.insert(
                        generation.clone(),
                        "ownership changed during observation".to_string(),
                    );
                    continue;
                }
                // Errors already fenced candidacy (or were explicitly released)
                // when the observation was committed above.
                if let Ok(Some(reason)) = evidence {
                    released_unverified_evidence.insert(generation.clone(), reason.to_string());
                }
                if !already_stopped.contains(generation) && !operations.stop(session) {
                    retirement_blockers.insert(
                        generation.clone(),
                        "authenticated idle stop or endpoint shutdown was not confirmed"
                            .to_string(),
                    );
                    continue;
                }
                if !already_stopped.contains(generation) {
                    operations.terminate_tunnel(session);
                }
                let endpoint = generations
                    .generations
                    .remove(generation)
                    .expect("generation was checked")
                    .endpoint;
                generations
                    .job_owners
                    .retain(|_, owner| !owner_matches_generation(owner, generation, &endpoint));
                if has_result_owners_for(&generations, generation, &endpoint) {
                    generations
                        .retired_evidence
                        .insert(generation.clone(), endpoint);
                }
                retired.push(generation.clone());
            } else {
                ownership_blockers.insert(generation.clone());
                retirement_blockers.insert(
                    generation.clone(),
                    "ownership changed before stop".to_string(),
                );
            }
        }
        write(runner_id, &generations)?;
        persist_retirement_verdicts(
            runner_id,
            &generations,
            &retirement_blockers,
            &evidence_blocked_generation_ids,
            &evidence_runs,
            &ownership_blockers,
            &retired,
        )?;
        Ok(retired)
    })?;
    // A release is only reported for generations that actually retired.
    released_unverified_evidence
        .retain(|generation, _| retired_generation_ids.contains(generation));
    released_stopped_generation_jobs
        .retain(|generation, _| retired_generation_ids.contains(generation));
    stopped_generation_job_owner_ids
        .retain(|generation, _| !retired_generation_ids.contains(generation));
    Ok(GenerationReconcileResult {
        retired_generation_ids,
        retirement_blockers,
        retained_evidence_generation_count: read(runner_id)?
            .map_or(0, |registry| registry.retired_evidence.len()),
        evidence_blocked_generation_ids,
        released_unverified_evidence,
        stopped_generation_job_owner_ids,
        released_stopped_generation_jobs,
    })
}

/// Locked entry point used only by the process-isolation test below.
///
/// Production reaches `record_authenticated_admission_locked` from inside an
/// already-held registry lock (see `reconcile`). This wrapper acquires the
/// lock itself, which is precisely what
/// `process_isolated_authenticated_admission_preserves_concurrent_job_admission`
/// spawns two processes to verify: that a concurrent job record cannot lose
/// accepted ownership while an admission holds the registry. Nothing in the
/// shipped binary calls it, so it is compiled out rather than carried.
#[cfg(test)]
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
    with_runner_registry_lock(runner_id, || {
        record_authenticated_admission_locked(runner_id, session, generation)
    })
}

pub(crate) fn record_authenticated_admission_locked(
    runner_id: &str,
    session: &RunnerSession,
    generation: &str,
) -> Result<()> {
    let mut generations =
        read(runner_id)?.unwrap_or_else(|| RollingGenerations::new(generation, session.clone()));
    #[cfg(test)]
    pause_authenticated_admission_after_read();
    if let Some(entry) = generations.generations.get_mut(generation) {
        // A reattached daemon gets a fresh local tunnel, so update the endpoint
        // without disturbing jobs already pinned to this lease.
        entry.endpoint = session.clone();
    } else {
        generations.begin(generation, session.clone());
    }
    generations.activate_preserving_drained(generation);
    write(runner_id, &generations)
}

/// A daemon restarted in place keeps its state directory, so every job, run
/// and artifact the stopped lease owned now belongs to the restarted lease:
/// the new daemon re-adopted them from the same store. Move the stopped
/// lease's ledger entry onto the restarted lease (and make it the admission
/// owner) instead of leaving a draining generation behind that observers
/// would report as dead or replaced. The caller holds the runner state lock.
pub(crate) fn adopt_restarted_lease_locked(
    runner_id: &str,
    previous_lease: &str,
    restarted_lease: &str,
    restarted: &RunnerSession,
) -> Result<()> {
    let mut generations = read(runner_id)?
        .unwrap_or_else(|| RollingGenerations::new(restarted_lease, restarted.clone()));
    rebind_restarted_lease_in(&mut generations, previous_lease, restarted_lease, restarted);
    write(runner_id, &generations)
}

fn rebind_restarted_lease_in(
    generations: &mut RollingGenerations<RunnerSession>,
    previous_lease: &str,
    restarted_lease: &str,
    restarted: &RunnerSession,
) {
    let previous_keys = generations
        .generations
        .iter()
        .filter(|(generation, entry)| {
            generation.as_str() != restarted_lease
                && owner_matches_generation(previous_lease, generation, &entry.endpoint)
        })
        .map(|(generation, _)| generation.clone())
        .collect::<Vec<_>>();
    for generation in &previous_keys {
        generations.generations.remove(generation);
    }
    let moved = |owner: &String| {
        owner == previous_lease || previous_keys.iter().any(|generation| generation == owner)
    };
    for owners in [
        &mut generations.job_owners,
        &mut generations.run_owners,
        &mut generations.artifact_owners,
    ] {
        for owner in owners.values_mut() {
            if moved(owner) {
                *owner = restarted_lease.to_string();
            }
        }
    }
    if moved(&generations.admission_owner) {
        // The admission owner must name a present generation before
        // activation can demote it.
        generations.admission_owner = restarted_lease.to_string();
    }
    generations.begin(restarted_lease, restarted.clone());
    let entry = generations
        .generations
        .get_mut(restarted_lease)
        .expect("restarted generation was inserted or found");
    entry.endpoint = restarted.clone();
    entry.observed_active_jobs = None;
    if generations.admission_owner == restarted_lease {
        entry.drain_state = crate::RollingDrainState::Admitting;
    } else {
        generations.activate_preserving_drained(restarted_lease);
    }
}

/// Seed a multi-generation ledger for the retained retirement and routing
/// tests. Production never creates a second generation: a refresh restarts the
/// daemon in place and rebinds its lease instead.
#[cfg(test)]
pub(crate) fn activate(
    runner_id: &str,
    current: &RunnerSession,
    generation: String,
    candidate: RunnerSession,
    draining_job_ids: &[String],
) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        let mut generations = read(runner_id)?.unwrap_or_else(|| {
            RollingGenerations::new(session_generation(current), current.clone())
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
            .unwrap_or_else(|| session_generation(current));
        // Owners may name the current generation by its lease rather than by
        // its registry key; fold them onto the key before rotation.
        let lease_owner = session_generation(current);
        if draining_owner != lease_owner {
            for owners in [
                &mut generations.job_owners,
                &mut generations.run_owners,
                &mut generations.artifact_owners,
            ] {
                for owner in owners.values_mut() {
                    if owner == &lease_owner {
                        *owner = draining_owner.clone();
                    }
                }
            }
        }
        // The daemon's live jobs may not all be in the ledger yet. Pin them to
        // the draining generation before activation so it cannot retire early.
        for job_id in draining_job_ids {
            generations
                .job_owners
                .insert(job_id.clone(), draining_owner.clone());
        }
        generations.begin(generation.clone(), candidate);
        generations.activate_preserving_drained(&generation);
        write(runner_id, &generations)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::{
        RunnerActiveJobState, RunnerSessionRole, RunnerSessionState, RunnerStatusReport,
        RunnerTunnelMode,
    };

    /// Seed a durable job owner on the generation serving `session`.
    /// Reconciliation and retirement still read these owners from persisted
    /// registries.
    fn seed_job_owner(runner_id: &str, session: &RunnerSession, job_id: &str) -> Result<()> {
        with_runner_registry_lock(runner_id, || {
            let mut generations = read(runner_id)?.unwrap_or_else(|| {
                RollingGenerations::new(session_generation(session), session.clone())
            });
            let owner = generations
                .generations
                .iter()
                .find_map(|(generation, entry)| {
                    (entry.endpoint.remote_daemon_lease_id == session.remote_daemon_lease_id)
                        .then(|| generation.clone())
                })
                .unwrap_or_else(|| generations.admission_owner.clone());
            generations.admit_job_for(&owner, job_id);
            write(runner_id, &generations)
        })
    }

    pub(super) fn session(lease: &str, endpoint: &str, tunnel_pid: Option<u32>) -> RunnerSession {
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

    /// A job whose submitting client disappeared must not pin its generation
    /// forever once its own endpoint reports the job finished.
    #[test]
    fn a_daemon_proven_terminal_job_releases_its_generation_ownership() {
        for status in ["succeeded", "failed", "cancelled"] {
            let record = serde_json::json!({"data": {"body": {"job": {"status": status}}}});
            assert!(job_record_is_terminal(&record), "{status} is terminal");
        }
    }

    /// Unknown fate is never settled: an unreachable endpoint, an unparsable
    /// record, or live work all keep the generation's ownership intact.
    #[test]
    fn a_job_without_proven_terminality_keeps_its_generation_ownership() {
        for record in [
            serde_json::json!({"data": {"body": {"job": {"status": "running"}}}}),
            serde_json::json!({"data": {"body": {"job": {"status": "queued"}}}}),
            serde_json::json!({"data": {"body": {"job": {"status": "not-a-status"}}}}),
            serde_json::json!({"data": {"body": {"job": {}}}}),
            serde_json::json!({}),
        ] {
            assert!(!job_record_is_terminal(&record), "{record} is not terminal");
        }
    }

    /// A result owner naming a generation this registry no longer knows is
    /// already unroutable. It must be reported, but it must not fence the
    /// retirement of unrelated generations whose ownership is consistent.
    #[test]
    fn a_dangling_result_owner_is_reported_without_fencing_consistent_generations() {
        let a = session("lease-a", "daemon-a", Some(101));
        let mut generations = RollingGenerations::new("lease-a".to_string(), a);
        generations
            .run_owners
            .insert("run-dangling".to_string(), "lease-removed".to_string());

        let audit = audit_ownership(&generations);

        assert!(
            !audit.fences_retirement,
            "an unroutable historical record must not block other retirements: {audit:?}"
        );
        assert_eq!(
            audit
                .retirement_blockers
                .get("lease-removed")
                .map(String::as_str),
            Some("ownership for run-dangling resolves to 0 live and 0 retired generations"),
        );
    }

    /// A job owner naming a generation this registry no longer knows is
    /// already unroutable. It must be reported, but it must not fence the
    /// retirement of unrelated generations whose ownership is consistent.
    #[test]
    fn a_dangling_job_owner_is_reported_without_fencing_consistent_generations() {
        let a = session("lease-a", "daemon-a", Some(101));
        let mut generations = RollingGenerations::new("lease-a".to_string(), a);
        generations
            .job_owners
            .insert("job-orphan".to_string(), "lease-removed".to_string());

        let audit = audit_ownership(&generations);

        assert!(
            !audit.fences_retirement,
            "an unroutable historical record must not block other retirements: {audit:?}"
        );
        assert_eq!(
            audit
                .retirement_blockers
                .get("lease-removed")
                .map(String::as_str),
            Some("ownership for job-orphan resolves to 0 live and 0 retired generations"),
        );
    }

    /// A job owner whose generation survives only as retained evidence is still
    /// the inspection and cancellation authority for work that may exist, so it
    /// fences retirement.
    #[test]
    fn a_job_owner_with_retired_evidence_and_no_live_generation_fences_retirement() {
        let a = session("lease-a", "daemon-a", Some(101));
        let mut generations = RollingGenerations::new("lease-a".to_string(), a);
        generations.retired_evidence.insert(
            "lease-removed".to_string(),
            session("lease-removed", "daemon-removed", Some(99)),
        );
        generations
            .job_owners
            .insert("job-orphan".to_string(), "lease-removed".to_string());

        let audit = audit_ownership(&generations);

        assert!(audit.fences_retirement, "{audit:?}");
        assert_eq!(
            audit
                .retirement_blockers
                .get("lease-removed")
                .map(String::as_str),
            Some("ownership for job-orphan resolves to 0 live and 1 retired generations"),
        );
    }

    /// An owner that resolves to more than one generation is unsafe to act on,
    /// because one identity would map to more than one endpoint.
    #[test]
    fn an_ambiguous_owner_fences_retirement() {
        let a = session("lease-a", "daemon-a", Some(101));
        let mut generations = RollingGenerations::new("lease-a".to_string(), a);
        generations.begin("lease-alias", session("lease-a", "daemon-alias", Some(102)));
        generations
            .job_owners
            .insert("job-ambiguous".to_string(), "lease-a".to_string());

        let audit = audit_ownership(&generations);

        assert!(audit.fences_retirement, "{audit:?}");
        assert_eq!(
            audit.retirement_blockers.get("lease-a").map(String::as_str),
            Some("ownership for job-ambiguous resolves to 2 live and 0 retired generations"),
        );
    }

    #[derive(Default)]
    struct FakeEndpointOperations {
        queued: RefCell<Vec<crate::RunnerJob>>,
        evidence: RefCell<std::collections::BTreeMap<String, serde_json::Value>>,
        evidence_calls: RefCell<Vec<(String, Vec<String>)>>,
        evidence_hook: Option<Box<dyn Fn()>>,
        evidence_unreachable_leases: RefCell<std::collections::BTreeSet<String>>,
        cancelled: RefCell<Vec<(String, String)>>,
        active_jobs: RefCell<std::collections::BTreeMap<String, usize>>,
        terminal_reconcile_failures: RefCell<std::collections::BTreeSet<String>>,
        stop_failures: RefCell<std::collections::BTreeSet<String>>,
        stopped_leases_proven: RefCell<std::collections::BTreeSet<String>>,
        terminal_reconciled_leases: RefCell<Vec<String>>,
        stopped_leases: RefCell<Vec<String>>,
        terminated_pids: RefCell<Vec<u32>>,
    }

    impl GenerationEndpointOperations for FakeEndpointOperations {
        fn evidence(
            &self,
            session: &RunnerSession,
            paths: &[String],
        ) -> Vec<Option<serde_json::Value>> {
            if let Some(hook) = &self.evidence_hook {
                hook();
            }
            let lease = session.remote_daemon_lease_id.as_deref().expect("lease");
            self.evidence_calls
                .borrow_mut()
                .push((lease.to_string(), paths.to_vec()));
            if self.evidence_unreachable_leases.borrow().contains(lease) {
                return vec![None; paths.len()];
            }
            paths
                .iter()
                .map(|path| self.evidence.borrow().get(path).cloned())
                .collect()
        }

        fn queued_jobs(&self, _session: &RunnerSession) -> Option<Vec<crate::RunnerJob>> {
            Some(self.queued.borrow().clone())
        }

        fn cancel_job(
            &self,
            session: &RunnerSession,
            job_id: &str,
            _request: &homeboy_core::api_jobs::RunnerJobCancelRequest,
        ) -> bool {
            self.cancelled.borrow_mut().push((
                session.remote_daemon_lease_id.clone().expect("lease"),
                job_id.to_string(),
            ));
            true
        }

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

        fn proven_stopped(&self, session: &RunnerSession) -> bool {
            self.stopped_leases_proven
                .borrow()
                .contains(session.remote_daemon_lease_id.as_deref().expect("lease"))
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
    fn queued_handoff_cancellation_replays_only_exact_durable_cancellation_to_its_generation() {
        let endpoint = session("lease-draining", "daemon-draining", Some(101));
        let operations = FakeEndpointOperations::default();
        let summary = serde_json::json!({
            "runner_id": endpoint.runner_id, "job_id": "cancelled-job", "operation": "runner_staged_execution",
            "status": "queued", "command": "homeboy agent-task run-plan", "source": "sealed-staging",
            "kind": "runner_staged_execution", "started_at_ms": 1, "updated_at_ms": 1,
            "elapsed_ms": 100_000, "heartbeat_age_ms": 100_000,
            "durable_run_id": "cancelled-run", "lifecycle": { "kind": "runner_staged_execution", "source": "sealed-staging", "durable_run_id": "cancelled-run" }
        });
        let response = serde_json::json!({"data":{"body":{"active_runner_jobs":[summary]}}});
        let job = queued_generation_jobs(&response, &endpoint)
            .expect("typed jobs")
            .pop()
            .expect("queued job");
        let mut active = job.clone();
        active.job_id = "active-run-job".to_string();
        active.durable_run_id = Some("active-run".to_string());
        active.lifecycle.as_mut().unwrap().durable_run_id = Some("active-run".to_string());
        let mut unbound = job.clone();
        unbound.job_id = "different-binding".to_string();
        unbound.lifecycle.as_mut().unwrap().durable_run_id = Some("other-run".to_string());
        let mut running = job.clone();
        running.job_id = "running-job".to_string();
        running.status = homeboy_core::api_jobs::JobStatus::Running;
        operations
            .queued
            .replace(vec![job, active, unbound, running]);

        reconcile_cancelled_queued_jobs(&endpoint, &operations, |run_id| run_id == "cancelled-run");

        assert_eq!(
            *operations.cancelled.borrow(),
            vec![("lease-draining".to_string(), "cancelled-job".to_string())]
        );
        assert!(operations.stopped_leases.borrow().is_empty());
        assert!(operations.terminated_pids.borrow().is_empty());
        assert!(
            queued_generation_jobs(&serde_json::json!({"data":{"body":{}}}), &endpoint).is_none()
        );
    }

    #[test]
    fn evidence_is_read_from_an_endpoint_that_is_running_other_jobs() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).expect("read request");
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(
                request.starts_with("GET /runs/terminal-run/artifacts "),
                "{request}"
            );
            let body = serde_json::json!({"data":{"body":{"artifacts":[]}}}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).expect("response");
        });
        // A busy admitting daemon still serves a terminal run's inventory.
        let mut endpoint = session("lease-busy", "daemon-busy", Some(101));
        endpoint.local_url = Some(format!("http://{address}"));
        let operations = HttpGenerationEndpointOperations {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client"),
        };
        assert_eq!(
            operations.evidence(&endpoint, &["/runs/terminal-run/artifacts".to_string()]),
            vec![Some(serde_json::json!({"data":{"body":{"artifacts":[]}}}))]
        );
        server.join().expect("fixture server");
    }

    #[test]
    fn retained_generation_cancellation_uses_real_http_and_reobserves_idle() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&cancelled);
        let server = std::thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().expect("request");
                let mut bytes = [0; 4096];
                let count = stream.read(&mut bytes).expect("read request");
                let request = String::from_utf8_lossy(&bytes[..count]);
                let body = if request.starts_with("GET /health ") {
                    serde_json::json!({"data":{"pid":42,"freshness":{"lease_id":"lease-http","active_jobs":usize::from(!observed.load(Ordering::SeqCst))}}})
                } else if request.starts_with("GET /jobs ") {
                    serde_json::json!({"data":{"body":{"active_runner_jobs":[{
                        "runner_id":"runner-a","job_id":"queued-http","operation":"runner_staged_execution","status":"queued",
                        "source":"sealed-staging","kind":"runner_staged_execution","command":"fixture","started_at_ms":1,"updated_at_ms":1,"elapsed_ms":1,"heartbeat_age_ms":1,
                        "durable_run_id":"cancelled-http","lifecycle":{"kind":"runner_staged_execution","source":"sealed-staging","durable_run_id":"cancelled-http"}
                    }]}}})
                } else {
                    assert!(
                        request.starts_with("POST /jobs/queued-http/cancel "),
                        "{request}"
                    );
                    observed.store(true, Ordering::SeqCst);
                    serde_json::json!({"data":{"body":{"job":{"status":"cancelled"}}}})
                };
                let body = body.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).expect("response");
            }
        });
        let mut endpoint = session("lease-http", "daemon-http", Some(101));
        endpoint.local_url = Some(format!("http://{address}"));
        let operations = HttpGenerationEndpointOperations {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client"),
        };
        reconcile_cancelled_queued_jobs(&endpoint, &operations, |run_id| {
            run_id == "cancelled-http"
        });
        assert_eq!(operations.active_jobs(&endpoint), Some(0));
        assert!(cancelled.load(Ordering::SeqCst));
        server.join().expect("fixture server");
    }

    fn idle_generation_with_unverifiable_evidence() {
        let idle = session("lease-idle", "daemon-idle", Some(101));
        let current = session("lease-current", "daemon-current", Some(303));
        let mut generations = RollingGenerations::new("lease-current".to_string(), current);
        generations.begin("lease-idle", idle);
        // A retained, nonterminal run is genuinely unverifiable.
        let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
        store
            .start_run_with_id(
                homeboy_core::observation::NewRunRecord::builder("runner-exec")
                    .metadata(serde_json::json!({"lab":{"runner":{"id":"runner-a"}}}))
                    .build(),
                "run-legacy".to_string(),
            )
            .unwrap();
        generations
            .run_owners
            .insert("run-legacy".to_string(), "lease-idle".to_string());
        write("runner-a", &generations).expect("persist generations");
    }

    #[test]
    fn recovery_only_agent_task_retires_using_ledger_ownership() {
        crate::test_support::with_isolated_home(|_| {
            use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("agent-task")
                        .metadata(serde_json::json!({"recovery":{"reason":"reconciled"}}))
                        .build(),
                )
                .expect("agent-task run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("terminal run");
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current", current);
            generations.begin("lease-idle", idle);
            generations
                .run_owners
                .insert(run.id.clone(), "lease-idle".into());
            write("runner-a", &generations).expect("persist generations");
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);
            let encoded = homeboy_core::execution_contract::encode_uri_component(&run.id);
            operations.evidence.borrow_mut().extend([
                (
                    format!("/runs/{encoded}/artifacts"),
                    serde_json::json!({"data":{"body":{"artifacts":[]}}}),
                ),
                (
                    format!("/runs/{encoded}/findings"),
                    serde_json::json!({"data":{"body":{"findings":[]}}}),
                ),
            ]);

            let result = reconcile_with("runner-a", &operations).expect("reconcile");
            assert_eq!(result.retired_generation_ids, ["lease-idle"]);
            assert!(result.evidence_blocked_generation_ids.is_empty());
            assert!(result.released_unverified_evidence.is_empty());
            let status = status_projection("runner-a").expect("status");
            assert_eq!(status.len(), 1);
            assert_eq!(status[0].generation, "lease-current");
        });
    }

    #[test]
    fn missing_controller_run_releases_evidence_without_attestation() {
        crate::test_support::with_isolated_home(|_| {
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current".to_string(), current);
            generations.begin("lease-idle", idle);
            generations
                .generations
                .get_mut("lease-idle")
                .unwrap()
                .observed_active_jobs = Some(0);
            generations
                .run_owners
                .insert("missing-run".into(), "lease-idle".into());
            generations
                .artifact_owners
                .insert("deleted-artifact".into(), "lease-idle".into());
            write("runner-a", &generations).unwrap();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);
            assert_eq!(
                persisted_retirement_verdicts("runner-a")
                    .unwrap()
                    .get("lease-idle"),
                None
            );
            let result = reconcile_with("runner-a", &operations).unwrap();
            assert_eq!(result.retired_generation_ids, ["lease-idle"]);
            assert!(result.evidence_blocked_generation_ids.is_empty());
            assert!(result.released_unverified_evidence.is_empty());
        });
    }

    #[test]
    fn status_reports_idle_stop_blocker_without_retrying_stop() {
        crate::test_support::with_isolated_home(|_| {
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current".to_string(), current);
            generations.begin("lease-idle", idle);
            write("runner-a", &generations).unwrap();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);
            operations
                .stop_failures
                .borrow_mut()
                .insert("lease-idle".into());
            let outcome = reconcile_with("runner-a", &operations).unwrap();
            assert!(outcome.retired_generation_ids.is_empty());
            let before = std::fs::read(path("runner-a").unwrap()).unwrap();
            assert_eq!(
                persisted_retirement_verdicts("runner-a").unwrap()["lease-idle"].verdict,
                GenerationRetirementDecision::Blocked {
                    kind: GenerationRetirementBlockerKind::StopUnconfirmed,
                    run_id: None,
                    reason: outcome.retirement_blockers["lease-idle"].clone(),
                }
            );
            assert_eq!(before, std::fs::read(path("runner-a").unwrap()).unwrap());
            assert_eq!(operations.stopped_leases.borrow().len(), 1);
        });
    }

    fn runner_mirror_retirement(mirror_bytes: Option<&[u8]>, expected_retired: bool) {
        crate::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
            let run = homeboy_core::observation::RunRecord {
                id: "mirrored-run".into(),
                kind: "agent-task".into(),
                status: "pass".into(),
                started_at: "2026-10-23T00:00:00Z".into(),
                finished_at: Some("2026-10-23T00:00:01Z".into()),
                metadata_json: serde_json::json!({"lab":{"runner":{"id":"runner-a"}}}),
                ..Default::default()
            };
            store.import_run(&run).unwrap();
            let path = home.path().join("source");
            std::fs::write(&path, b"verified evidence").unwrap();
            let source = store
                .record_artifact_with_id(
                    &run.id,
                    "evidence",
                    &path,
                    "source-id",
                    serde_json::json!({}),
                )
                .unwrap();
            // Replace the source record with the projection's remote reference.
            store.delete_artifact_record(&source.id).unwrap();
            let mut reference = source.clone();
            reference.artifact_type = "remote_file".into();
            reference.path = "runner-artifact://source-id".into();
            store.import_artifact(&reference).unwrap();
            if let Some(bytes) = mirror_bytes {
                std::fs::write(&path, bytes).unwrap();
                store
                    .record_artifact_with_id(
                        &run.id,
                        "evidence",
                        &path,
                        "mirror-id",
                        serde_json::json!({"agent_task":{"projection":"runner_mirrored"}}),
                    )
                    .unwrap();
            }
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current".to_string(), current);
            generations.begin("lease-idle", idle);
            generations
                .run_owners
                .insert(run.id.clone(), "lease-idle".into());
            generations
                .artifact_owners
                .insert(reference.id, "lease-idle".into());
            write("runner-a", &generations).unwrap();
            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .extend([("lease-idle".into(), 0), ("lease-current".into(), 0)]);
            operations.evidence.borrow_mut().extend([
                (
                    "/runs/mirrored-run/artifacts".into(),
                    serde_json::json!({"data":{"body":{"artifacts":[source]}}}),
                ),
                (
                    "/runs/mirrored-run/findings".into(),
                    serde_json::json!({"data":{"body":{"findings":[]}}}),
                ),
            ]);
            let result = reconcile_with("runner-a", &operations).unwrap();
            if expected_retired {
                assert_eq!(result.retired_generation_ids, ["lease-idle"]);
            } else {
                assert!(result.retired_generation_ids.is_empty());
                assert!(result.retirement_blockers["lease-idle"]
                    .contains("matching runner_mirrored file is missing"));
            }
        });
    }

    #[test]
    fn runner_mirrored_remote_file_is_retirable() {
        runner_mirror_retirement(Some(b"verified evidence"), true);
    }

    #[test]
    fn remote_file_without_mirror_blocks_retirement() {
        runner_mirror_retirement(None, false);
    }

    #[test]
    fn remote_file_with_different_sha256_mirror_blocks_retirement() {
        runner_mirror_retirement(Some(b"different content"), false);
    }

    #[test]
    fn controller_directory_artifact_is_verified_by_tree_classification() {
        crate::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
            let run = homeboy_core::observation::RunRecord {
                id: "directory-run".into(),
                kind: "lint".into(),
                status: "pass".into(),
                started_at: "2026-10-23T00:00:00Z".into(),
                finished_at: Some("2026-10-23T00:00:01Z".into()),
                metadata_json: serde_json::json!({"lab":{"runner":{"id":"runner-a"}}}),
                ..Default::default()
            };
            store.import_run(&run).unwrap();
            let source = home.path().join("directory-source");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("evidence.txt"), b"verified evidence").unwrap();
            let artifact = store
                .record_directory_artifact(&run.id, "evidence", &source)
                .unwrap();
            assert_eq!(artifact.artifact_type, "directory");
            // The same local path must not make an unowned reference count as
            // retained controller evidence. Classification precedes hashing.
            let mut remote_reference = artifact.clone();
            remote_reference.artifact_type = "remote_file".into();
            assert!(matches!(
                homeboy_core::artifact_links::classify_retained_artifact_availability(
                    &remote_reference
                )
                .unwrap(),
                homeboy_core::artifact_links::RetainedArtifactAvailability::Missing { .. }
            ));
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current".to_string(), current);
            generations.begin("lease-idle", idle);
            generations
                .run_owners
                .insert(run.id.clone(), "lease-idle".into());
            generations
                .artifact_owners
                .insert(artifact.id.clone(), "lease-idle".into());
            write("runner-a", &generations).unwrap();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);
            operations.evidence.borrow_mut().extend([
                (
                    "/runs/directory-run/artifacts".into(),
                    serde_json::json!({"data":{"body":{"artifacts":[artifact.clone()]}}}),
                ),
                (
                    "/runs/directory-run/findings".into(),
                    serde_json::json!({"data":{"body":{"findings":[]}}}),
                ),
            ]);
            // Tree verification is not an exemption: altered bytes block.
            std::fs::write(
                std::path::Path::new(&artifact.path).join("evidence.txt"),
                b"tampered",
            )
            .unwrap();
            let result = reconcile_with("runner-a", &operations).unwrap();
            assert!(result.retired_generation_ids.is_empty());
            assert!(result
                .retirement_blockers
                .get("lease-idle")
                .as_deref()
                .unwrap()
                .contains("directory evidence is unavailable"));
            std::fs::write(
                std::path::Path::new(&artifact.path).join("evidence.txt"),
                b"verified evidence",
            )
            .unwrap();
            let result = reconcile_with("runner-a", &operations).unwrap();
            assert_eq!(result.retired_generation_ids, ["lease-idle"]);
        });
    }

    #[test]
    fn stopped_generation_evidence_is_verified_through_the_admitting_endpoint() {
        crate::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
            let run = homeboy_core::observation::RunRecord {
                id: "stopped-run".into(),
                kind: "lint".into(),
                status: "pass".into(),
                started_at: "2026-10-23T00:00:00Z".into(),
                finished_at: Some("2026-10-23T00:00:01Z".into()),
                metadata_json: serde_json::json!({"lab":{"runner":{"id":"runner-a"}}}),
                ..Default::default()
            };
            store.import_run(&run).unwrap();
            let source = home.path().join("stopped-source");
            std::fs::write(&source, b"verified evidence").unwrap();
            let artifact = store.record_artifact(&run.id, "log", &source).unwrap();
            let stopped = session("lease-stopped", "daemon-stopped", Some(101));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations = RollingGenerations::new("lease-current".to_string(), current);
            generations.begin("lease-stopped", stopped);
            generations
                .run_owners
                .insert(run.id.clone(), "lease-stopped".into());
            write("runner-a", &generations).unwrap();
            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-current".to_string(), 0);
            operations
                .stopped_leases_proven
                .borrow_mut()
                .insert("lease-stopped".into());
            operations
                .evidence_unreachable_leases
                .borrow_mut()
                .insert("lease-stopped".into());
            operations.evidence.borrow_mut().extend([
                (
                    "/runs/stopped-run/artifacts".into(),
                    serde_json::json!({"data":{"body":{"artifacts":[artifact]}}}),
                ),
                (
                    "/runs/stopped-run/findings".into(),
                    serde_json::json!({"data":{"body":{"findings":[]}}}),
                ),
            ]);
            let result = reconcile_with("runner-a", &operations).unwrap();
            assert_eq!(result.retired_generation_ids, ["lease-stopped"]);
            assert!(result.evidence_blocked_generation_ids.is_empty());
            assert!(result.released_unverified_evidence.is_empty());
        });
    }

    /// #15653: by default an idle generation whose retained evidence cannot be
    /// verified stays registered and is reported as evidence-blocked.
    #[test]
    fn stopped_generation_releases_only_remote_evidence_checks() {
        for (proven_stopped, status, tampered, blocker) in [
            (true, "pass", false, None),
            (
                false,
                "pass",
                false,
                Some("source artifact inventory unavailable"),
            ),
            (
                true,
                "running",
                false,
                Some("controller run is not terminal"),
            ),
            (true, "pass", true, Some("artifact")),
        ] {
            crate::test_support::with_isolated_home(|home| {
                let store =
                    homeboy_core::observation::ObservationStore::open_initialized().unwrap();
                let run = homeboy_core::observation::RunRecord {
                    id: "stopped-run".into(),
                    kind: "lint".into(),
                    status: status.into(),
                    started_at: "2026-10-23T00:00:00Z".into(),
                    finished_at: Some("2026-10-23T00:00:01Z".into()),
                    metadata_json: serde_json::json!({"lab":{"runner":{"id":"runner-a"}}}),
                    ..Default::default()
                };
                store.import_run(&run).unwrap();
                let source = home.path().join("stopped-source");
                std::fs::write(&source, b"verified evidence").unwrap();
                let artifact = store.record_artifact(&run.id, "log", &source).unwrap();
                if tampered {
                    std::fs::write(&artifact.path, b"tampered").unwrap();
                }
                let stopped = session("lease-stopped", "daemon-stopped", Some(101));
                let current = session("lease-current", "daemon-current", Some(303));
                let mut generations = RollingGenerations::new("lease-current".to_string(), current);
                generations.begin("lease-stopped", stopped);
                generations
                    .run_owners
                    .insert(run.id.clone(), "lease-stopped".into());
                write("runner-a", &generations).unwrap();
                let operations = FakeEndpointOperations::default();
                operations
                    .active_jobs
                    .borrow_mut()
                    .insert("lease-current".to_string(), 0);
                if proven_stopped {
                    operations
                        .stopped_leases_proven
                        .borrow_mut()
                        .insert("lease-stopped".into());
                }
                let result = reconcile_with("runner-a", &operations).unwrap();
                if let Some(blocker) = blocker {
                    assert!(result.retired_generation_ids.is_empty(), "{result:?}");
                    assert!(
                        result.retirement_blockers["lease-stopped"].contains(blocker),
                        "{result:?}"
                    );
                    assert!(result.released_unverified_evidence.is_empty());
                } else {
                    assert_eq!(result.retired_generation_ids, ["lease-stopped"]);
                    assert!(result.evidence_blocked_generation_ids.is_empty());
                    assert!(result.released_unverified_evidence["lease-stopped"]
                        .contains("daemon proven stopped"));
                }
            });
        }
    }

    /// #15653: by default an idle generation whose retained evidence cannot be
    /// verified stays registered and is reported as evidence-blocked.
    #[test]
    fn unverifiable_evidence_blocks_retirement_by_default_and_is_classified() {
        crate::test_support::with_isolated_home(|_| {
            idle_generation_with_unverifiable_evidence();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);

            let result = reconcile_with("runner-a", &operations).expect("reconcile");

            assert!(result.retired_generation_ids.is_empty(), "{result:?}");
            assert!(result.released_unverified_evidence.is_empty());
            assert!(result
                .evidence_blocked_generation_ids
                .contains("lease-idle"));
            assert!(result
                .retirement_blockers
                .get("lease-idle")
                .is_some_and(|reason| reason.contains("retained evidence run-legacy")));
            assert!(operations.stopped_leases.borrow().is_empty());
            let verdict = &persisted_retirement_verdicts("runner-a").unwrap()["lease-idle"];
            assert!(chrono::DateTime::parse_from_rfc3339(&verdict.checked_at).is_ok());
            assert_eq!(
                verdict.verdict,
                GenerationRetirementDecision::Blocked {
                    kind: GenerationRetirementBlockerKind::RetainedEvidence,
                    run_id: Some("run-legacy".into()),
                    reason: result.retirement_blockers["lease-idle"].clone(),
                }
            );
            // Status reports the operation's verdict even if the local evidence changes.
            homeboy_core::observation::ObservationStore::open_initialized()
                .unwrap()
                .finish_run(
                    "run-legacy",
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .unwrap();
            assert_eq!(
                &persisted_retirement_verdicts("runner-a").unwrap()["lease-idle"],
                verdict
            );
        });
    }

    fn reconcile_evidence_with_concurrent_registry_writer(change_owner: bool) {
        crate::test_support::with_isolated_home(|_| {
            idle_generation_with_unverifiable_evidence();
            homeboy_core::observation::ObservationStore::open_initialized()
                .unwrap()
                .finish_run(
                    "run-legacy",
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .unwrap();
            let operations = FakeEndpointOperations {
                evidence_hook: Some(Box::new(move || {
                    with_runner_registry_lock("runner-a", || {
                        if change_owner {
                            let mut registry = read("runner-a")?.expect("registry");
                            registry
                                .run_owners
                                .insert("run-legacy".into(), "lease-current".into());
                            write("runner-a", &registry)?;
                        }
                        Ok(())
                    })
                    .expect("evidence reads must not hold the registry lock");
                })),
                ..Default::default()
            };
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);
            operations.evidence.borrow_mut().extend([
                (
                    "/runs/run-legacy/artifacts".into(),
                    serde_json::json!({"data":{"body":{"artifacts": []}}}),
                ),
                (
                    "/runs/run-legacy/findings".into(),
                    serde_json::json!({"data":{"body":{"findings": []}}}),
                ),
            ]);
            let result = reconcile_with("runner-a", &operations).expect("reconcile");
            assert!(!operations.evidence_calls.borrow().is_empty());
            if change_owner {
                assert!(result.retired_generation_ids.is_empty(), "{result:?}");
                assert_eq!(
                    result.retirement_blockers["lease-idle"],
                    "ownership changed during observation"
                );
                assert!(operations.stopped_leases.borrow().is_empty());
            } else {
                assert_eq!(result.retired_generation_ids, ["lease-idle"], "{result:?}");
            }
        });
    }

    #[test]
    fn reconcile_evidence_allows_concurrent_registry_writer() {
        reconcile_evidence_with_concurrent_registry_writer(false);
    }

    #[test]
    fn reconcile_rejects_evidence_after_run_ownership_changes() {
        reconcile_evidence_with_concurrent_registry_writer(true);
    }

    /// #15653: the explicit release retires the idle generation and keeps its
    /// run ownership as retired evidence. Nothing is deleted.
    #[test]
    fn release_unverified_evidence_retires_and_keeps_provenance() {
        crate::test_support::with_isolated_home(|_| {
            idle_generation_with_unverifiable_evidence();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-current".to_string(), 0),
            ]);

            let result = reconcile_with_operations(
                "runner-a",
                &operations,
                GenerationReconcilePolicy {
                    release_unverified_evidence: true,
                    ..Default::default()
                },
            )
            .expect("reconcile with release");

            assert_eq!(result.retired_generation_ids, ["lease-idle"]);
            assert_eq!(
                persisted_retirement_verdicts("runner-a").unwrap()["lease-idle"].verdict,
                GenerationRetirementDecision::Ready
            );
            assert!(result
                .released_unverified_evidence
                .get("lease-idle")
                .is_some_and(|reason| reason.contains("retained evidence run-legacy")));
            assert!(!result.retirement_blockers.contains_key("lease-idle"));
            assert_eq!(
                operations.stopped_leases.borrow().as_slice(),
                ["lease-idle"]
            );
            let registry = read("runner-a").expect("read").expect("registry");
            assert!(!registry.generations.contains_key("lease-idle"));
            assert_eq!(
                registry.run_owners.get("run-legacy").map(String::as_str),
                Some("lease-idle"),
                "run ownership is preserved"
            );
            assert!(
                registry.retired_evidence.contains_key("lease-idle"),
                "the endpoint is kept as retired evidence provenance"
            );
        });
    }

    /// The release never overrides live work: a busy generation stays.
    #[test]
    fn release_unverified_evidence_never_retires_a_busy_generation() {
        crate::test_support::with_isolated_home(|_| {
            idle_generation_with_unverifiable_evidence();
            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 2),
                ("lease-current".to_string(), 0),
            ]);

            let result = reconcile_with_operations(
                "runner-a",
                &operations,
                GenerationReconcilePolicy {
                    release_unverified_evidence: true,
                    ..Default::default()
                },
            )
            .expect("reconcile");

            assert!(result.retired_generation_ids.is_empty());
            assert!(result.released_unverified_evidence.is_empty());
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    /// Relaxing the global fence for a dangling job owner must still retire only
    /// generations that pass the per-generation idle, stop, and evidence gates.
    /// A verified-idle draining generation may retire; a generation with live or
    /// unproven work must be retained.
    #[test]
    fn a_dangling_job_owner_lets_verified_idle_generations_retire_without_bypassing_per_generation_gates(
    ) {
        crate::test_support::with_isolated_home(|_| {
            let idle = session("lease-idle", "daemon-idle", Some(101));
            let busy = session("lease-busy", "daemon-busy", Some(202));
            let current = session("lease-current", "daemon-current", Some(303));
            let mut generations =
                RollingGenerations::new("lease-current".to_string(), current.clone());
            generations.begin("lease-idle", idle);
            generations.begin("lease-busy", busy);
            generations
                .job_owners
                .insert("job-orphan".to_string(), "lease-removed".to_string());
            write("runner-a", &generations).expect("persist generations");

            let operations = FakeEndpointOperations::default();
            operations.active_jobs.borrow_mut().extend([
                ("lease-idle".to_string(), 0),
                ("lease-busy".to_string(), 1),
                ("lease-current".to_string(), 0),
            ]);

            let result =
                reconcile_with("runner-a", &operations).expect("reconcile with dangling job owner");

            assert!(
                !result.retired_generation_ids.is_empty(),
                "verified-idle draining generations must still retire: {result:?}"
            );
            assert_eq!(result.retired_generation_ids, ["lease-idle"]);
            assert_eq!(
                result
                    .retirement_blockers
                    .get("lease-removed")
                    .map(String::as_str),
                Some("ownership for job-orphan resolves to 0 live and 0 retired generations"),
            );
            assert_eq!(
                result
                    .retirement_blockers
                    .get("lease-busy")
                    .map(String::as_str),
                Some("1 active jobs"),
            );
            assert_eq!(
                operations.stopped_leases.borrow().as_slice(),
                ["lease-idle"]
            );

            let registry = read("runner-a").expect("read registry").expect("registry");
            assert!(
                !registry.generations.contains_key("lease-idle"),
                "verified-idle draining generation must retire"
            );
            assert!(
                registry.generations.contains_key("lease-busy"),
                "a generation with live work must be retained"
            );
            assert!(registry.generations.contains_key("lease-current"));
        });
    }

    #[test]
    fn concurrent_status_projections_preserve_stale_direct_generation_state_without_operations() {
        crate::test_support::with_isolated_home(|_| {
            let draining = session("lease-draining", "daemon-draining", Some(101));
            let active = session("lease-active", "daemon-active", Some(202));
            seed_job_owner("runner-a", &draining, "draining-job").expect("record draining job");
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
                    std::thread::spawn(move || {
                        barrier.wait();
                        status_projection("runner-a").expect("status projection")
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
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            seed_job_owner("runner-a", &a, "job-a").expect("record A job");
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
    fn reconciled_zero_for_admission_owner_retires_unclaimed_jobs() {
        crate::test_support::with_isolated_home(|_| {
            let current = session("lease-current", "daemon-current", Some(202));
            seed_job_owner("runner-a", &current, "job-a").expect("record first job");
            seed_job_owner("runner-a", &current, "job-b").expect("record second job");
            crate::admission_fence::record_admitted_job("runner-a", "job-a").expect("admit job-a");
            crate::admission_fence::record_admitted_job("runner-a", "job-b").expect("admit job-b");
            // The idle observation must start after both admissions.
            std::thread::sleep(Duration::from_millis(5));
            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-current".to_string(), 0);

            reconcile_with("runner-a", &operations).expect("reconcile authoritative remote zero");

            let projection = status_projection("runner-a").expect("projection");
            assert_eq!(projection[0].active_job_count, 0);
            assert_eq!(projection[0].observed_active_job_count, Some(0));
            assert!(
                status_job_owners("runner-a").expect("owners")[0]
                    .job_ids
                    .is_empty(),
                "durable jobs with no live daemon owner are residue"
            );
            crate::admission_fence::with_admission_fence("runner-a", "connect", |fence| {
                assert!(fence.is_none(), "authoritative zero must permit reconnect");
                Ok(())
            })
            .expect("released fence");
        });
    }

    #[test]
    fn live_daemon_owner_keeps_its_durable_job() {
        crate::test_support::with_isolated_home(|_| {
            let current = session("lease-current", "daemon-current", Some(202));
            seed_job_owner("runner-a", &current, "job-live").expect("record live job");
            crate::admission_fence::record_admitted_job("runner-a", "job-live")
                .expect("admit live job");
            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-current".to_string(), 1);

            reconcile_with("runner-a", &operations).expect("reconcile live owner");

            assert_eq!(
                status_job_owners("runner-a").expect("owners")[0].job_ids,
                ["job-live"]
            );
            crate::admission_fence::with_admission_fence("runner-a", "connect", |fence| {
                assert!(fence.is_some(), "a live job remains a placement fence");
                Ok(())
            })
            .expect("live fence");
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
        crate::test_support::with_isolated_home(|_| {
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

            assert!(read("runner-a")
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
        registry
            .generations
            .activate_preserving_drained("lease-next");
        assert!(registry.generations.generations.contains_key("lease-fresh"));
        assert_eq!(
            registry.generations.job_owner("accepted-during-reconcile"),
            Some("lease-fresh")
        );
    }

    #[test]
    #[ignore = "invoked by process_isolated_authenticated_admission_preserves_concurrent_job_admission"]
    fn process_record_authenticated_admission() {
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        record_authenticated_admission("runner-a", &fresh).expect("record authenticated admission");
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
    #[ignore = "invoked by process_isolated_reconciliation_preserves_concurrent_job_admission"]
    fn process_record_fresh_job() {
        let fresh = session("lease-fresh", "daemon-fresh", Some(202));
        seed_job_owner("runner-a", &fresh, "accepted-during-reconcile")
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

            fn proven_stopped(&self, _: &RunnerSession) -> bool {
                false
            }

            fn stop(&self, _: &RunnerSession) -> bool {
                true
            }

            fn terminate_tunnel(&self, _: &RunnerSession) {}
        }

        reconcile_with("runner-a", &ProcessBlockingOperations(process_sync_dir()))
            .expect("reconcile after remote drain check");
    }

    #[test]
    fn a_registry_without_runner_identity_is_rejected_unchanged() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let mut prior = RollingGenerations::new("lease-a", a);
            assert!(prior.admit_job_for("lease-a", "job-a"));
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &prior,
            )
            .expect("write registry without runner identity");
            let before = std::fs::read(path("runner-a").expect("registry path"))
                .expect("read registry bytes");

            let error = read("runner-a").expect_err("a registry without runner_id is rejected");
            assert_eq!(error.code.as_str(), "config.invalid_json");
            assert_eq!(
                std::fs::read(path("runner-a").expect("registry path"))
                    .expect("reread registry bytes"),
                before,
                "a rejected registry is never rewritten"
            );
        });
    }

    #[test]
    fn registry_rejects_an_explicit_mismatched_runner_id() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            homeboy_core::engine::local_files::write_json_file(
                &path("runner-a").expect("registry path"),
                &GenerationRegistry {
                    runner_id: "runner-b".to_string(),
                    generations: RollingGenerations::new("lease-a", a),
                },
            )
            .expect("write registry");

            let error = read("runner-a").expect_err("mismatched registry rejects");
            assert_eq!(error.code.as_str(), "config.invalid_value");
            assert_eq!(error.details["value"], "runner-b");
            assert!(error.details["problem"]
                .as_str()
                .expect("problem")
                .contains("runner-scoped path `runner-a`"));
        });
    }

    #[test]
    fn malformed_prior_registry_is_not_migrated() {
        crate::test_support::with_isolated_home(|_| {
            let registry_path = path("runner-a").expect("registry path");
            std::fs::create_dir_all(registry_path.parent().expect("registry directory"))
                .expect("create registry directory");
            std::fs::write(&registry_path, r#"{"generations": []}"#)
                .expect("write malformed registry");

            let error = read("runner-a").expect_err("malformed registry rejects");
            assert_eq!(error.code.as_str(), "config.invalid_json");
            assert_eq!(
                std::fs::read_to_string(registry_path).expect("read malformed registry"),
                r#"{"generations": []}"#
            );
        });
    }

    #[test]
    fn unsupported_registry_shape_fails_closed() {
        crate::test_support::with_isolated_home(|_| {
            let registry_path = path("runner-a").expect("registry path");
            std::fs::create_dir_all(registry_path.parent().expect("registry directory"))
                .expect("create registry directory");
            std::fs::write(
                &registry_path,
                r#"{"runner_id":"runner-a","unexpected":true}"#,
            )
            .expect("write unsupported registry");

            let error = read("runner-a").expect_err("unsupported registry rejects");
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
        crate::test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "127.0.0.1:63114", Some(101));
            let fresh = session("lease-fresh", "127.0.0.1:50575", Some(202));
            seed_job_owner("runner-a", &stale, "active-stale-job").expect("record stale job");

            reconcile_admission_session("runner-a", &fresh).expect("promote fresh admission");
            seed_job_owner("runner-a", &fresh, "fresh-cook-job")
                .expect("record one fresh dispatch");

            assert_eq!(
                admission_session("runner-a").expect("resolve admission"),
                Some(fresh.clone()),
                "new Cook work uses the same endpoint runner status just verified"
            );
            let projection = status_projection("runner-a").expect("projection");
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
    fn retained_runs_read_one_evidence_batch_per_endpoint() {
        crate::test_support::with_isolated_home(|_| {
            use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
            let store = ObservationStore::open_initialized().expect("store");
            let endpoint = session("lease-owner", "daemon-owner", Some(101));
            let admitting = session("lease-admitting", "daemon-admitting", Some(202));
            let mut generations = RollingGenerations::new("lease-admitting", admitting);
            generations.begin("lease-owner", endpoint.clone());
            let operations = FakeEndpointOperations::default();
            for index in 0..4 {
                let id = format!("retained-{index}");
                store
                    .start_run_with_id(
                        NewRunRecord::builder("runner_execution")
                            .metadata(serde_json::json!({"runner_id":"runner-a"}))
                            .build(),
                        id.clone(),
                    )
                    .expect("run");
                store
                    .finish_run(&id, RunStatus::Pass, None)
                    .expect("terminal");
                generations
                    .run_owners
                    .insert(id.clone(), "lease-owner".to_string());
                for (kind, body) in [
                    (
                        "artifacts",
                        serde_json::json!({"data":{"body":{"artifacts":[]}}}),
                    ),
                    (
                        "findings",
                        serde_json::json!({"data":{"body":{"findings":[]}}}),
                    ),
                ] {
                    operations
                        .evidence
                        .borrow_mut()
                        .insert(format!("/runs/{id}/{kind}"), body);
                }
            }
            // Exercise both the owner-only and owner-to-admitting batch paths.
            for unreachable in [false, true] {
                operations.evidence_calls.borrow_mut().clear();
                if unreachable {
                    operations
                        .evidence_unreachable_leases
                        .borrow_mut()
                        .insert("lease-owner".to_string());
                }
                assert_eq!(
                    verify_retained_evidence(
                        "runner-a",
                        &generations,
                        "lease-owner",
                        &endpoint,
                        &operations
                    )
                    .expect("verified"),
                    None
                );
                let calls = operations.evidence_calls.borrow();
                assert_eq!(calls.len(), if unreachable { 2 } else { 1 });
                assert_eq!(calls[0].0, "lease-owner");
                assert_eq!(calls[0].1.len(), 8);
                if unreachable {
                    assert_eq!(calls[1].0, "lease-admitting");
                    assert_eq!(calls[1].1, calls[0].1);
                }
            }
        });
    }

    #[test]
    fn retained_run_local_blocker_prevents_all_evidence_reads() {
        crate::test_support::with_isolated_home(|_| {
            use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
            let store = ObservationStore::open_initialized().expect("store");
            let endpoint = session("lease-a", "daemon-a", Some(101));
            let mut generations = RollingGenerations::new("lease-a", endpoint.clone());
            let operations = FakeEndpointOperations::default();
            // The valid run sorts first, so an interleaved implementation would
            // already have read evidence before encountering the local blocker.
            for id in ["a-valid", "z-blocked"] {
                store
                    .start_run_with_id(
                        NewRunRecord::builder("runner_execution")
                            .metadata(serde_json::json!({"runner_id":"runner-a"}))
                            .build(),
                        id.to_string(),
                    )
                    .expect("run");
                generations
                    .run_owners
                    .insert(id.to_string(), "lease-a".to_string());
            }
            store
                .finish_run("a-valid", RunStatus::Pass, None)
                .expect("terminal");
            operations.evidence.borrow_mut().insert(
                "/runs/a-valid/artifacts".to_string(),
                serde_json::json!({"data":{"body":{"artifacts":[]}}}),
            );
            operations.evidence.borrow_mut().insert(
                "/runs/a-valid/findings".to_string(),
                serde_json::json!({"data":{"body":{"findings":[]}}}),
            );
            let error = verify_retained_evidence(
                "runner-a",
                &generations,
                "lease-a",
                &endpoint,
                &operations,
            )
            .expect_err("local blocker");
            assert!(
                error.message.contains("controller run is not terminal"),
                "{error:?}"
            );
            assert!(operations.evidence_calls.borrow().is_empty());
        });
    }

    #[test]
    fn retained_runner_exec_evidence_accepts_only_matching_top_level_runner_id() {
        crate::test_support::with_isolated_home(|_| {
            use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
            let store = ObservationStore::open_initialized().expect("store");
            let endpoint = session("lease-a", "daemon-a", Some(101));
            let mut generations = RollingGenerations::new("lease-a", endpoint.clone());
            let operations = FakeEndpointOperations::default();
            for owner in ["runner-a", "different-runner"] {
                let run = store
                    .start_run(
                        NewRunRecord::builder("runner_execution")
                            .metadata(
                                serde_json::json!({"kind": "runner_exec", "runner_id": owner}),
                            )
                            .build(),
                    )
                    .expect("run");
                store
                    .finish_run(&run.id, RunStatus::Pass, None)
                    .expect("terminal run");
                generations.run_owners.clear();
                generations
                    .run_owners
                    .insert(run.id.clone(), "lease-a".to_string());
                let encoded = homeboy_core::execution_contract::encode_uri_component(&run.id);
                operations.evidence.borrow_mut().insert(
                    format!("/runs/{encoded}/artifacts"),
                    serde_json::json!({"data":{"body":{"artifacts":[]}}}),
                );
                operations.evidence.borrow_mut().insert(
                    format!("/runs/{encoded}/findings"),
                    serde_json::json!({"data":{"body":{"findings":[]}}}),
                );
                let result = verify_retained_evidence(
                    "runner-a",
                    &generations,
                    "lease-a",
                    &endpoint,
                    &operations,
                );
                if owner == "runner-a" {
                    result.expect("top-level runner identity accepted");
                } else {
                    assert!(result
                        .expect_err("mismatched runner")
                        .message
                        .contains("runner identity is inconsistent"));
                }
            }
        });
    }

    #[test]
    fn reconciliation_reports_owner_counts_and_observed_counts() {
        crate::test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            let mut generations = RollingGenerations::new("lease-fresh", fresh.clone());
            generations.begin("lease-stale", stale);
            write("runner-a", &generations).expect("persist generations");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-stale".to_string(), 2);
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-fresh".to_string(), 1);
            reconcile_with("runner-a", &operations).expect("reconcile live count");

            let projection = status_projection("runner-a").expect("projection");
            let projected_fresh = projection
                .iter()
                .find(|entry| entry.generation == "lease-fresh")
                .expect("admitting generation status");
            assert_eq!(projected_fresh.active_job_count, 0);
            assert_eq!(projected_fresh.observed_active_job_count, Some(1));
            assert!(projected_fresh.active_job_count_authoritative);
            let projected_stale = projection
                .iter()
                .find(|entry| entry.generation == "lease-stale")
                .expect("draining generation status");
            assert_eq!(projected_stale.active_job_count, 0);
            assert_eq!(projected_stale.observed_active_job_count, Some(2));
            assert!(projected_stale.active_job_count_authoritative);
        });
    }

    #[test]
    fn unreachable_draining_generation_keeps_unverified_persisted_claims() {
        crate::test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            let mut generations = RollingGenerations::new("lease-fresh", fresh.clone());
            generations.begin("lease-stale", stale);
            generations
                .job_owners
                .insert("job-stale".to_string(), "lease-stale".to_string());
            write("runner-a", &generations).expect("persist generations");

            let operations = FakeEndpointOperations::default();
            let result =
                reconcile_with("runner-a", &operations).expect("reconcile unreachable endpoint");

            let projection = status_projection("runner-a").expect("projection");
            let stale = projection
                .iter()
                .find(|entry| entry.generation == "lease-stale")
                .expect("draining generation status");
            assert_eq!(
                stale.active_job_count, 1,
                "status follows the durable owner"
            );
            assert_eq!(stale.observed_active_job_count, None);
            assert!(!stale.active_job_count_authoritative);
            assert_eq!(stale.job_owner_count, 1);
            assert!(result.retired_generation_ids.is_empty());
            assert_eq!(
                result
                    .retirement_blockers
                    .get("lease-stale")
                    .map(String::as_str),
                Some("endpoint identity/active work unavailable; stop is not authorized")
            );
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    #[test]
    fn unclaimed_draining_projection_retires_when_live_daemon_is_idle() {
        crate::test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            seed_job_owner("runner-a", &stale, "job-stale").expect("record stale job");
            activate(
                "runner-a",
                &stale,
                "lease-fresh".to_string(),
                fresh.clone(),
                &["job-stale".to_string()],
            )
            .expect("activate live generation");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-fresh".to_string(), 0);
            let result = reconcile_with("runner-a", &operations)
                .expect("resolve unclaimed draining projection");

            assert_eq!(result.retired_generation_ids, ["lease-stale"]);
            let projection = status_projection("runner-a").expect("projection");
            assert_eq!(projection.len(), 1);
            assert_eq!(projection[0].generation, "lease-fresh");
            assert_eq!(projection[0].active_job_count, 0);
            assert_eq!(projection[0].observed_active_job_count, Some(0));
            assert!(status_job_owners("runner-a")
                .expect("owners")
                .iter()
                .all(|owner| owner.job_ids.is_empty()));
            let report = RunnerStatusReport {
                runner_id: "runner-a".to_string(),
                connected: true,
                state: RunnerSessionState::Connected,
                session: Some(fresh.clone()),
                stale_daemon: None,
                configured_job_binary_build_identity: None,
                daemon_freshness: None,
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                stale_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::Available,
                active_job_source: None,
                active_job_error: None,
                active_job_recovery_evidence: None,
                session_path: "test".to_string(),
            };
            let owners = status_job_owners("runner-a").expect("owners");
            let summary = report.admission_summary_with_generations(&projection, &owners, 0);
            assert!(
                !summary.accepting_jobs,
                "a connected session without a live lease observation is not admission-ready"
            );
            assert!(summary.unresolved_generation_ids.is_empty());
            assert_eq!(summary.unresolved_retained_projection_count, 0);
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    #[test]
    fn reachable_terminal_settlement_failure_is_not_overridden_by_an_idle_live_daemon() {
        crate::test_support::with_isolated_home(|_| {
            let stale = session("lease-stale", "daemon-stale", Some(101));
            let fresh = session("lease-fresh", "daemon-fresh", Some(202));
            seed_job_owner("runner-a", &stale, "job-stale").expect("record stale job");
            activate(
                "runner-a",
                &stale,
                "lease-fresh".to_string(),
                fresh.clone(),
                &["job-stale".to_string()],
            )
            .expect("activate live generation");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-fresh".to_string(), 0);
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-stale".to_string(), 0);
            operations
                .terminal_reconcile_failures
                .borrow_mut()
                .insert("lease-stale".to_string());
            reconcile_with("runner-a", &operations)
                .expect("reachable settlement failure stays fail-closed");

            let projection = status_projection("runner-a").expect("projection");
            assert!(projection.iter().any(|entry| {
                entry.generation == "lease-stale"
                    && entry.active_job_count == 1
                    && entry.observed_active_job_count.is_none()
            }));
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    #[test]
    fn proven_stopped_draining_generation_retires_without_touching_live_owner() {
        crate::test_support::with_isolated_home(|_| {
            let stopped = session("lease-stopped", "daemon-stopped", Some(101));
            let live = session("lease-live", "daemon-live", Some(202));
            activate(
                "runner-a",
                &stopped,
                "lease-live".to_string(),
                live.clone(),
                &[],
            )
            .expect("activate live generation");

            let operations = FakeEndpointOperations::default();
            operations
                .stopped_leases_proven
                .borrow_mut()
                .insert("lease-stopped".to_string());
            // A proven-stopped generation is never observed (#15685): its
            // stop proof and empty owner set stand in for an observed zero.
            let result =
                reconcile_with("runner-a", &operations).expect("retire proven stopped generation");

            assert_eq!(
                result.retired_generation_ids,
                vec!["lease-stopped".to_string()]
            );
            let projection = status_projection("runner-a").expect("projection");
            assert_eq!(projection.len(), 1);
            assert_eq!(projection[0].generation, "lease-live");
            assert!(operations.stopped_leases.borrow().is_empty());
            assert!(operations.terminated_pids.borrow().is_empty());
        });
    }

    /// A proven-stopped draining generation that still owns a job its dead
    /// daemon can never settle (#15779): stopped generation `lease-stopped`
    /// with `job-stuck`, live admission owner `lease-live` with `job-live`.
    fn stopped_generation_with_stuck_job(proven_stopped: bool) -> FakeEndpointOperations {
        let stopped = session("lease-stopped", "daemon-stopped", Some(101));
        let live = session("lease-live", "daemon-live", Some(202));
        seed_job_owner("runner-a", &stopped, "job-stuck").expect("record stuck job");
        activate(
            "runner-a",
            &stopped,
            "lease-live".to_string(),
            live.clone(),
            &["job-stuck".to_string()],
        )
        .expect("activate live generation");
        seed_job_owner("runner-a", &live, "job-live").expect("record live job");
        let operations = FakeEndpointOperations::default();
        if proven_stopped {
            operations
                .stopped_leases_proven
                .borrow_mut()
                .insert("lease-stopped".to_string());
        }
        operations
            .active_jobs
            .borrow_mut()
            .insert("lease-live".to_string(), 1);
        operations
    }

    fn owned_job_ids(runner_id: &str, generation: &str) -> Vec<String> {
        status_job_owners(runner_id)
            .expect("job owners")
            .into_iter()
            .find(|owners| owners.generation == generation)
            .map(|owners| owners.job_ids)
            .unwrap_or_default()
    }

    #[test]
    fn stopped_generation_job_owner_blocks_by_default_and_names_attestation() {
        crate::test_support::with_isolated_home(|_| {
            let operations = stopped_generation_with_stuck_job(true);

            let result = reconcile_with("runner-a", &operations).expect("reconcile");

            assert!(result.retired_generation_ids.is_empty());
            assert_eq!(
                result
                    .retirement_blockers
                    .get("lease-stopped")
                    .map(String::as_str),
                Some("daemon proven stopped; 1 job owner(s) need workload-absence attestation")
            );
            assert_eq!(
                result.stopped_generation_job_owner_ids.get("lease-stopped"),
                Some(&vec!["job-stuck".to_string()])
            );
            assert!(result.released_stopped_generation_jobs.is_empty());
            assert_eq!(owned_job_ids("runner-a", "lease-stopped"), ["job-stuck"]);
        });
    }

    #[test]
    fn attested_release_retires_stopped_generation_and_keeps_live_owner() {
        crate::test_support::with_isolated_home(|_| {
            let operations = stopped_generation_with_stuck_job(true);

            let result = reconcile_with_operations(
                "runner-a",
                &operations,
                GenerationReconcilePolicy {
                    release_stopped_generation_jobs: true,
                    ..Default::default()
                },
            )
            .expect("attested reconcile");

            assert_eq!(result.retired_generation_ids, ["lease-stopped"]);
            assert_eq!(
                result.released_stopped_generation_jobs.get("lease-stopped"),
                Some(&vec!["job-stuck".to_string()])
            );
            assert!(result.stopped_generation_job_owner_ids.is_empty());
            assert!(!result.retirement_blockers.contains_key("lease-stopped"));
            let projection = status_projection("runner-a").expect("projection");
            assert_eq!(projection.len(), 1);
            assert_eq!(projection[0].generation, "lease-live");
            assert_eq!(owned_job_ids("runner-a", "lease-live"), ["job-live"]);
            // An already-stopped daemon is never asked to stop again.
            assert!(operations.stopped_leases.borrow().is_empty());
        });
    }

    #[test]
    fn attestation_never_releases_a_generation_not_proven_stopped() {
        crate::test_support::with_isolated_home(|_| {
            let operations = stopped_generation_with_stuck_job(false);

            let result = reconcile_with_operations(
                "runner-a",
                &operations,
                GenerationReconcilePolicy {
                    release_stopped_generation_jobs: true,
                    ..Default::default()
                },
            )
            .expect("reconcile");

            assert!(result.retired_generation_ids.is_empty());
            assert!(result.released_stopped_generation_jobs.is_empty());
            assert_eq!(
                result
                    .retirement_blockers
                    .get("lease-stopped")
                    .map(String::as_str),
                Some("endpoint identity/active work unavailable; stop is not authorized")
            );
            assert_eq!(owned_job_ids("runner-a", "lease-stopped"), ["job-stuck"]);
        });
    }

    #[test]
    fn persisted_generations_route_jobs_project_status_and_retire_once() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            // With no generations file yet, the first recorded job seeds
            // generation A from its session.
            assert_eq!(admission_session("runner-a").expect("admission"), None);
            seed_job_owner("runner-a", &a, "job-a").expect("record A job");

            // Candidate B has already passed startup/tunnel validation. Only
            // this activation moves admission; a failed candidate leaves A intact.
            let before_candidate_activation = read("runner-a").expect("read A").expect("A ledger");
            assert_eq!(before_candidate_activation.admission_owner, "lease-a");
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("activate validated B");
            seed_job_owner("runner-a", &b, "job-b").expect("record B job");

            // Reloading the persisted ledger simulates a controller restart.
            let reloaded = read("runner-a").expect("reload").expect("ledger");
            assert_eq!(reloaded.admission_owner, "build-b");
            assert_eq!(reloaded.job_owner("job-a"), Some("lease-a"));
            assert_eq!(reloaded.job_owner("job-b"), Some("build-b"));
            assert_eq!(
                admission_session("runner-a").expect("route admission"),
                Some(b.clone())
            );

            let projected = status_projection("runner-a").expect("status projection");
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
            let owners = status_job_owners("runner-a").expect("owner projection");
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
            let serialized = serde_json::to_value(report).expect("serialize captured status");
            assert!(
                serialized.get("generations").is_none(),
                "serialization must not fetch historical generation state"
            );
            assert_eq!(
                serde_json::to_value(
                    status_projection("runner-a").expect("explicit generation inventory")
                )
                .unwrap(),
                serde_json::to_value(&projected).unwrap()
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
                    active_child_count: None,
                    active_cell_count: None,
                    re_adoptable: false,
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
            assert_eq!(summary.live_daemon_job_count, Some(1));
            assert_eq!(summary.retained_durable_job_count, 2);
            assert_eq!(summary.active_job_count, Some(1));
            assert_eq!(summary.unresolved_retained_projection_count, 1);
            assert!(summary.admission_blocking_job_ids.is_empty());
            assert_eq!(summary.unresolved_generation_ids, ["lease-a"]);

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 1);
            reconcile_with("runner-a", &operations).expect("busy A remains");
            assert_eq!(status_projection("runner-a").expect("projection").len(), 2);

            operations
                .active_jobs
                .borrow_mut()
                .insert("lease-a".to_string(), 0);
            reconcile_with("runner-a", &operations).expect("retire A");
            reconcile_with("runner-a", &operations).expect("idempotent reconcile");
            assert_eq!(operations.stopped_leases.into_inner(), vec!["lease-a"]);
            assert_eq!(operations.terminated_pids.into_inner(), vec![101]);
            let final_projection = status_projection("runner-a").expect("final projection");
            assert_eq!(final_projection.len(), 1);
            assert_eq!(final_projection[0].generation, "build-b");
            assert_eq!(final_projection[0].active_job_count, 1);
        });
    }

    #[test]
    fn reconciliation_rebuilds_draining_counts_and_retires_terminal_generations() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let c = session("lease-c", "daemon-c", Some(303));

            seed_job_owner("runner-a", &a, "job-a").expect("record A job");
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["job-a".to_string()],
            )
            .expect("promote B");
            seed_job_owner("runner-a", &b, "job-b").expect("record B job");
            activate(
                "runner-a",
                &b,
                "build-c".to_string(),
                c.clone(),
                &["job-b".to_string()],
            )
            .expect("promote C");
            seed_job_owner("runner-a", &c, "job-c").expect("record C job");

            let operations = FakeEndpointOperations::default();
            operations
                .active_jobs
                .borrow_mut()
                .extend([("lease-a".to_string(), 0), ("lease-b".to_string(), 1)]);
            let first = reconcile_with("runner-a", &operations).expect("reconcile terminal A");
            assert_eq!(first.retired_generation_ids, ["lease-a"]);

            let projected = status_projection("runner-a").expect("project after A");
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
            let second = reconcile_with("runner-a", &operations).expect("reconcile terminal B");
            assert_eq!(second.retired_generation_ids, ["build-b"]);
            let projected = status_projection("runner-a").expect("final projection");
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
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            seed_job_owner("runner-a", &a, "job-a").expect("record A job");
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

            reconcile_with("runner-a", &operations).expect("failed settlement remains fail-closed");

            let projected = status_projection("runner-a").expect("projection");
            assert!(projected
                .iter()
                .any(|entry| entry.generation == "lease-a" && entry.active_job_count == 1));
            assert!(operations.stopped_leases.borrow().is_empty());
            assert!(operations.terminated_pids.borrow().is_empty());
        });
    }

    #[test]
    fn second_rotation_keeps_jobs_owned_by_the_named_draining_generation() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));
            let c = session("lease-c", "daemon-c", Some(303));

            activate("runner-a", &a, "build-b".to_string(), b.clone(), &[]).expect("promote B");
            seed_job_owner("runner-a", &b, "job-b").expect("record B job");

            activate(
                "runner-a",
                &b,
                "build-c".to_string(),
                c.clone(),
                &["job-b".to_string()],
            )
            .expect("promote C");

            let generations = read("runner-a")
                .expect("read generations")
                .expect("generation registry");
            assert_eq!(generations.job_owner("job-b"), Some("build-b"));
            assert_eq!(generations.admission_owner, "build-c");
        });
    }

    #[test]
    fn first_rotation_pins_active_a_jobs_before_b_can_retire_it() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon-a", Some(101));
            let b = session("lease-b", "daemon-b", Some(202));

            // No ledger exists yet, but the daemon reports authoritative
            // active job identities.
            activate(
                "runner-a",
                &a,
                "build-b".to_string(),
                b.clone(),
                &["active-job-a".to_string()],
            )
            .expect("activate B without losing active A work");

            let ledger = read("runner-a").expect("read ledger").expect("ledger");
            assert_eq!(ledger.job_owner("active-job-a"), Some("lease-a"));
            assert_eq!(
                admission_session("runner-a").expect("route admissions"),
                Some(b)
            );
        });
    }

    #[test]
    fn in_place_restart_rebinds_the_stopped_lease_without_leaving_a_generation_behind() {
        crate::test_support::with_isolated_home(|_| {
            let a = session("lease-a", "daemon", Some(101));
            let b = session("lease-b", "daemon", Some(202));
            seed_job_owner("runner-a", &a, "job-a").expect("record job on lease A");
            let mut generations = read("runner-a").expect("read").expect("ledger");
            generations
                .run_owners
                .insert("run-a".to_string(), "lease-a".to_string());
            generations
                .artifact_owners
                .insert("artifact-a".to_string(), "lease-a".to_string());
            write("runner-a", &generations).expect("record result owners");

            with_runner_registry_lock("runner-a", || {
                adopt_restarted_lease_locked("runner-a", "lease-a", "lease-b", &b)
            })
            .expect("adopt restarted lease");

            let generations = read("runner-a").expect("read").expect("ledger");
            assert_eq!(
                generations.generations.keys().collect::<Vec<_>>(),
                vec!["lease-b"],
                "no draining generation of the stopped lease is left behind"
            );
            assert_eq!(generations.admission_owner, "lease-b");
            assert_eq!(generations.job_owner("job-a"), Some("lease-b"));
            assert_eq!(generations.run_owners["run-a"], "lease-b");
            assert_eq!(generations.artifact_owners["artifact-a"], "lease-b");
            assert_eq!(
                generations.generations["lease-b"].drain_state,
                crate::RollingDrainState::Admitting
            );
            assert_eq!(admission_session("runner-a").expect("admission"), Some(b));
            let owners = status_job_owners("runner-a").expect("status owners");
            assert!(owners
                .iter()
                .all(|owner| owner.job_ids.is_empty() || owner.generation == "lease-b"));
        });
    }
}
