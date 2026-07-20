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
    let Some(mut generations) = read(runner_id, legacy)? else {
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
    for (generation, session) in drained {
        if operations.stop(&session) {
            if let Some(pid) = session.tunnel_pid {
                operations.terminate_tunnel(pid);
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

pub(crate) fn record_job_run(
    runner_id: &str,
    legacy: &RunnerSession,
    job_id: &str,
    run_id: &str,
) -> Result<()> {
    let Some(mut generations) = read(runner_id, Some(legacy))? else {
        return Ok(());
    };
    generations.record_run(job_id, run_id);
    write(runner_id, &generations)
}

pub(crate) fn record_job_artifacts(
    runner_id: &str,
    legacy: &RunnerSession,
    job_id: &str,
    artifact_ids: impl IntoIterator<Item = String>,
) -> Result<()> {
    let Some(mut generations) = read(runner_id, Some(legacy))? else {
        return Ok(());
    };
    for artifact_id in artifact_ids {
        generations.record_artifact(job_id, artifact_id);
    }
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
}

/// Remove an unactivated candidate from the durable ledger. This is safe to
/// call after any candidate validation failure and never changes admission.
pub(crate) fn rollback_candidate(
    runner_id: &str,
    legacy: &RunnerSession,
    generation: &str,
) -> Result<()> {
    let Some(mut generations) = read(runner_id, Some(legacy))? else {
        return Ok(());
    };
    if generations.rollback(generation) {
        write(runner_id, &generations)?;
    }
    Ok(())
}

/// Undo a candidate that was activated locally but could not be durably
/// published as the controller session. The previous admission remains intact.
pub(crate) fn rollback_activation(
    runner_id: &str,
    current: &RunnerSession,
    generation: &str,
) -> Result<()> {
    let Some(mut generations) = read(runner_id, Some(current))? else {
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
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

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
