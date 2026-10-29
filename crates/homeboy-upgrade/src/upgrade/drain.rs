//! Controller-only admission window for upgrade promotion.
use super::{admission::with_controller_upgrade_admission, operation::UpgradeOperation};
use homeboy_core::{
    runtime_promotion::{begin_upgrade_drain, UpgradeDrainGuard},
    Error, Result,
};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct UpgradeDrainOptions {
    pub timeout: Duration,
    /// Never inferred from --force: cancelling shared work needs explicit consent.
    pub cancel_active: bool,
}
impl Default for UpgradeDrainOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::ZERO,
            cancel_active: false,
        }
    }
}

pub(super) struct FleetDrainGuard {
    local: Option<UpgradeDrainGuard>,
}
impl FleetDrainGuard {
    pub(super) fn resume(&mut self) -> Result<()> {
        self.local.take();
        Ok(())
    }
}

fn local_live_jobs() -> Result<Vec<homeboy_core::api_jobs::Job>> {
    let path = homeboy_core::paths::observation_db()?;
    if !homeboy_core::api_jobs::JobStore::exists_at_path(&path) {
        return Ok(Vec::new());
    }
    let store = homeboy_core::api_jobs::JobStore::open_without_reconciliation(path)?;
    Ok(store
        .list()
        .into_iter()
        .filter(|job| !job.status.is_terminal())
        .collect())
}

fn blocking_workflows(jobs: &[homeboy_core::api_jobs::Job]) -> Result<Vec<String>> {
    if !jobs
        .iter()
        .any(|job| job.operation == "controller.workflow")
    {
        return Ok(Vec::new());
    }
    let store = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
        homeboy_core::paths::observation_db()?,
    )?;
    Ok(homeboy_core::runtime_promotion::upgrade_blocking_workflows(
        &store, jobs,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::{
        api_jobs::{JobStatus, JobStore},
        test_support::with_isolated_home,
    };

    #[test]
    fn live_durable_job_blocks_drain_until_the_store_clears() {
        with_isolated_home(|_| {
            let store = JobStore::open_without_reconciliation(
                homeboy_core::paths::observation_db().unwrap(),
            )
            .unwrap();
            let job = store.create("test.live-upgrade-job");
            let mut operation = UpgradeOperation::start_durable("homeboy upgrade").unwrap();
            let started = Instant::now();
            let blocked = drain(&mut operation, &[], true, UpgradeDrainOptions::default())
                .err()
                .expect("live job blocks");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "refusal must not sleep"
            );
            assert_eq!(blocked.details["kind"], "upgrade_drain_timeout");
            assert_eq!(
                blocked.details["controller_jobs"][0]["id"],
                job.id.to_string()
            );
            assert!(
                homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(Some("new-cook"),)
                    .is_ok()
            );
            let hints = serde_json::to_string(&blocked.hints).unwrap();
            assert!(hints.contains("homeboy upgrade --cancel-active"), "{hints}");
            assert!(!hints.contains("--force"), "{hints}");
            assert_eq!(store.get(job.id).unwrap().status, JobStatus::Queued);
            let finish = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                store.cancel(job.id, "job finished during fixture").unwrap();
            });
            let guard = drain(
                &mut operation,
                &[],
                true,
                UpgradeDrainOptions {
                    timeout: Duration::from_secs(3),
                    cancel_active: false,
                },
            )
            .unwrap();
            finish.join().unwrap();
            drop(guard);
        });
    }

    #[test]
    fn upgrade_drain_timeout_names_blocking_workflow_and_recovery() {
        with_isolated_home(|home| {
            use homeboy_core::daemon::workflow_job::{
                WorkflowJobDriver, WorkflowJobRequest, WORKFLOW_JOB_REQUEST_SCHEMA,
            };
            let definition =
                homeboy_core::workflow::WorkflowDefinition::from_value(serde_json::json!({
                    "schema": "homeboy/workflow/v1", "id": "blocking-batch",
                    "steps": [{"id": "work", "exec": {"program": "/bin/true"}}]
                }))
                .unwrap();
            let inputs = definition.resolve_inputs(&Default::default()).unwrap();
            let instance = definition.instance_id(&inputs, None);
            let request = WorkflowJobRequest {
                schema: WORKFLOW_JOB_REQUEST_SCHEMA.into(),
                instance: instance.clone(),
                instance_name: None,
                definition_digest: definition.digest(),
                definition,
                inputs,
                homeboy_program: "/bin/true".into(),
                cwd: home.path().display().to_string(),
                parent: None,
                env: Default::default(),
                homeboy_env: Default::default(),
                programs: Default::default(),
            };
            let _harness = homeboy_core::test_support::ControllerJobHarness::durable_queued(
                std::sync::Arc::new(WorkflowJobDriver),
                serde_json::to_value(request).unwrap(),
                &homeboy_core::paths::observation_db().unwrap(),
                "workflow-drain-lease",
            )
            .unwrap();
            let mut operation = UpgradeOperation::start_durable("homeboy upgrade").unwrap();
            let error = drain(&mut operation, &[], true, UpgradeDrainOptions::default())
                .err()
                .unwrap();
            assert_eq!(error.details["workflows"], serde_json::json!([instance]));
            assert!(serde_json::to_string(&error.hints)
                .unwrap()
                .contains(&format!("homeboy workflow cancel {instance}")));
        });
    }

    #[test]
    fn selected_busy_runner_does_not_join_controller_drain() {
        with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start_durable("homeboy upgrade").unwrap();
            let started = Instant::now();
            let guard = drain(
                &mut operation,
                &["lab-with-live-job".to_string()],
                false,
                UpgradeDrainOptions {
                    timeout: Duration::from_secs(3),
                    cancel_active: true,
                },
            )
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(1));
            let run = homeboy_core::observation::ObservationStore::open_initialized()
                .unwrap()
                .get_run(operation.id().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(run.metadata_json["phase"], "draining_controller");
            assert!(run.metadata_json["cancelled"].is_null());
            drop(guard);
        });
    }

    #[test]
    fn explicit_cancel_flag_cancels_real_jobs_and_retains_exact_identity() {
        with_isolated_home(|_| {
            let path = homeboy_core::paths::observation_db().unwrap();
            let store = JobStore::open_without_reconciliation(&path).unwrap();
            let job = store.create("test.cancel-upgrade-job");
            let mut operation =
                UpgradeOperation::start_durable("homeboy upgrade --cancel-active").unwrap();
            let guard = drain(
                &mut operation,
                &[],
                true,
                UpgradeDrainOptions {
                    cancel_active: true,
                    ..UpgradeDrainOptions::default()
                },
            )
            .unwrap();
            assert_eq!(
                JobStore::open_without_reconciliation(path)
                    .unwrap()
                    .get(job.id)
                    .unwrap()
                    .status,
                JobStatus::Cancelled
            );
            let run = homeboy_core::observation::ObservationStore::open_initialized()
                .unwrap()
                .get_run(operation.id().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                run.metadata_json["cancelled"],
                serde_json::json!([format!("controller_job:{}", job.id)])
            );
            drop(guard);
        });
    }
}

pub(super) fn drain(
    operation: &mut UpgradeOperation,
    _targets: &[String],
    _skip_runners: bool,
    options: UpgradeDrainOptions,
) -> Result<FleetDrainGuard> {
    operation.set_phase_durable("reserving_upgrade_drain")?;
    let admission = super::controller_upgrade_admission()?;
    let runs: Vec<String> = admission
        .blockers
        .iter()
        .map(|blocker| blocker.run_id.clone())
        .collect();
    let guard = begin_upgrade_drain(operation.id().expect("durable upgrade"), runs)?;
    let guard = FleetDrainGuard { local: Some(guard) };
    operation.set_phase_durable("draining_controller")?;
    // Cancellation is asynchronous: allow it to land even without an opt-in wait.
    let timeout = if options.cancel_active && options.timeout.is_zero() {
        Duration::from_secs(120)
    } else {
        options.timeout
    };
    let started = Instant::now();
    let mut cancelled = BTreeSet::new();
    loop {
        let admission = super::controller_upgrade_admission()?;
        let local_jobs = local_live_jobs()?;
        if admission.blockers.is_empty() && local_jobs.is_empty() {
            return Ok(guard);
        }
        let workflow_blockers = blocking_workflows(&local_jobs)?;
        super::operation::emit_upgrade_phase(&format!(
            "draining elapsed={}s budget={}s cooks=[{}] controller_jobs=[{}] workflows=[{}]",
            started.elapsed().as_secs(),
            timeout.as_secs(),
            admission
                .blockers
                .iter()
                .map(|blocker| blocker.run_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            local_jobs
                .iter()
                .map(|job| job.id.to_string())
                .collect::<Vec<_>>()
                .join(","),
            workflow_blockers
                .iter()
                .map(|instance| format!("{instance}: homeboy workflow cancel {instance}"))
                .collect::<Vec<_>>()
                .join(",")
        ));
        if options.cancel_active {
            for job in &local_jobs {
                let key = format!("controller_job:{}", job.id);
                if !cancelled.contains(&key) {
                    if job.operation.starts_with("controller.") {
                        homeboy_core::daemon::LocalControllerJobClient::connect_existing_job(
                            &job.id.to_string(),
                        )?
                        .cancel(&job.id.to_string(), "explicit upgrade --cancel-active")?;
                    } else {
                        homeboy_core::api_jobs::JobStore::open_without_reconciliation(
                            homeboy_core::paths::observation_db()?,
                        )?
                        .cancel(job.id, "explicit upgrade --cancel-active")?;
                    }
                    operation.record_cancelled_durable(&key)?;
                    cancelled.insert(key);
                }
            }
            for blocker in &admission.blockers {
                let key = format!("cook:{}", blocker.run_id);
                if !cancelled.contains(&key) {
                    with_controller_upgrade_admission(|provider| {
                        provider.cancel_for_upgrade(&blocker.run_id)
                    })?;
                    operation.record_cancelled_durable(&key)?;
                    cancelled.insert(key);
                }
            }
        }
        if started.elapsed() >= timeout {
            let commands: Vec<String> = admission
                .blockers
                .iter()
                .map(|blocker| blocker.recovery_command.clone())
                .chain(
                    workflow_blockers
                        .iter()
                        .map(|instance| format!("homeboy workflow cancel {instance}")),
                )
                .chain(
                    (!local_jobs.is_empty() && !options.cancel_active)
                        .then(|| "homeboy upgrade --cancel-active".to_string()),
                )
                .collect();
            let mut error = Error::validation_invalid_argument(
                "upgrade_drain",
                "upgrade refused: live work present; no binaries were replaced",
                None,
                None,
            );
            for command in commands {
                error = error.with_hint(command);
            }
            error.details["kind"] = serde_json::json!("upgrade_drain_timeout");
            error.details["cooks"] = serde_json::json!(admission.blockers);
            error.details["controller_jobs"] = serde_json::json!(local_jobs);
            error.details["workflows"] = serde_json::json!(workflow_blockers);
            error.details["cancelled"] = serde_json::json!(cancelled);
            return Err(error);
        }
        std::thread::sleep(Duration::from_secs(1).min(timeout.saturating_sub(started.elapsed())));
    }
}
