//! One admission window for controller and selected runner promotion (#16056).
use super::{
    admission::with_controller_upgrade_admission, operation::UpgradeOperation,
    runner_upgrade_provider::with_runner_upgrade,
};
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
            timeout: Duration::from_secs(1800),
            cancel_active: false,
        }
    }
}

pub(super) struct FleetDrainGuard {
    local: Option<UpgradeDrainGuard>,
    runners: Vec<String>,
    operation: String,
    runs: Vec<String>,
}
impl FleetDrainGuard {
    pub(super) fn resume(&mut self) -> Result<()> {
        with_runner_upgrade(|provider| {
            provider.set_upgrade_drain(&self.runners, &self.operation, &self.runs, true)
        })?;
        self.local.take();
        with_runner_upgrade(|provider| provider.verify_upgrade_admission(&self.runners))?;
        self.runners.clear();
        Ok(())
    }
}
impl Drop for FleetDrainGuard {
    fn drop(&mut self) {
        let _ = self.resume();
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
            let blocked = drain(
                &mut operation,
                &[],
                true,
                UpgradeDrainOptions {
                    timeout: Duration::ZERO,
                    cancel_active: false,
                },
            )
            .err()
            .expect("live job blocks");
            assert_eq!(blocked.details["kind"], "upgrade_drain_timeout");
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
                    timeout: Duration::from_secs(3),
                    cancel_active: true,
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
    targets: &[String],
    skip_runners: bool,
    options: UpgradeDrainOptions,
) -> Result<FleetDrainGuard> {
    operation.set_phase_durable("reserving_upgrade_drain")?;
    let runners = if skip_runners {
        Vec::new()
    } else {
        with_runner_upgrade(|provider| provider.drain_targets(targets))?
    };
    let admission = super::controller_upgrade_admission()?;
    let runs: Vec<String> = admission
        .blockers
        .iter()
        .map(|blocker| blocker.run_id.clone())
        .collect();
    let guard = begin_upgrade_drain(
        operation.id().expect("durable upgrade"),
        runs.clone(),
        runners.clone(),
    )?;
    let guard = FleetDrainGuard {
        local: Some(guard),
        runners: runners.clone(),
        operation: operation.id().expect("durable upgrade").to_string(),
        runs,
    };
    with_runner_upgrade(|provider| {
        provider.set_upgrade_drain(&runners, &guard.operation, &guard.runs, false)
    })?;
    operation.set_phase_durable("draining_controller_and_runners")?;
    let started = Instant::now();
    let mut cancelled = BTreeSet::new();
    loop {
        let admission = super::controller_upgrade_admission()?;
        let jobs = with_runner_upgrade(|provider| provider.drain_jobs(&runners))?;
        let local_jobs = local_live_jobs()?;
        if admission.blockers.is_empty() && jobs.is_empty() && local_jobs.is_empty() {
            return Ok(guard);
        }
        super::operation::emit_upgrade_phase(&format!(
            "draining elapsed={}s budget={}s cooks=[{}] runner_jobs={jobs:?} controller_jobs={local_jobs:?}",
            started.elapsed().as_secs(),
            options.timeout.as_secs(),
            admission
                .blockers
                .iter()
                .map(|blocker| blocker.run_id.as_str())
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
            for (runner, job) in &jobs {
                let key = format!("runner:{runner}/job:{job}");
                if !cancelled.contains(&key) {
                    with_runner_upgrade(|provider| provider.cancel_drain_job(runner, job))?;
                    operation.record_cancelled_durable(&key)?;
                    cancelled.insert(key);
                }
            }
        }
        if started.elapsed() >= options.timeout {
            let commands: Vec<String> = admission
                .blockers
                .iter()
                .map(|blocker| blocker.recovery_command.clone())
                .chain(
                    jobs.iter()
                        .map(|(runner, job)| format!("homeboy runner job cancel {runner} {job}")),
                )
                .chain(
                    (!local_jobs.is_empty() && !options.cancel_active)
                        .then(|| "homeboy upgrade --cancel-active".to_string()),
                )
                .collect();
            let mut error = Error::validation_invalid_argument(
                "upgrade_drain",
                "bounded upgrade drain timed out; no binaries were replaced",
                None,
                None,
            );
            for command in commands {
                error = error.with_hint(command);
            }
            error.details["kind"] = serde_json::json!("upgrade_drain_timeout");
            error.details["cooks"] = serde_json::json!(admission.blockers);
            error.details["runner_jobs"] = serde_json::json!(jobs);
            error.details["controller_jobs"] = serde_json::json!(local_jobs);
            error.details["cancelled"] = serde_json::json!(cancelled);
            return Err(error);
        }
        std::thread::sleep(
            Duration::from_secs(1).min(options.timeout.saturating_sub(started.elapsed())),
        );
    }
}
