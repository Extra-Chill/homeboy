//! One admission window for controller and selected runner promotion (#16056).
use super::remote_drains::release_upgrade_drains;
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
            timeout: Duration::ZERO,
            cancel_active: false,
        }
    }
}

pub(super) struct FleetDrainGuard {
    local: Option<UpgradeDrainGuard>,
    runners: Vec<String>,
    operation: String,
}
impl FleetDrainGuard {
    pub(super) fn resume(&mut self) -> Result<()> {
        release_upgrade_drains(&self.operation)?;
        self.local.take();
        with_runner_upgrade(|provider| provider.verify_upgrade_admission(&self.runners))?;
        self.runners.clear();
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
                homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(
                    Some("new-cook"),
                    None,
                )
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
    };
    operation.reserve_remote_drains(&runners, &runs)?;
    operation.set_phase_durable("draining_controller_and_runners")?;
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
        let jobs = with_runner_upgrade(|provider| provider.drain_jobs(&runners))?;
        let local_jobs = local_live_jobs()?;
        if admission.blockers.is_empty() && jobs.is_empty() && local_jobs.is_empty() {
            return Ok(guard);
        }
        super::operation::emit_upgrade_phase(&format!(
            "draining elapsed={}s budget={}s cooks=[{}] runner_jobs={jobs:?} controller_jobs={local_jobs:?}",
            started.elapsed().as_secs(),
            timeout.as_secs(),
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
        if started.elapsed() >= timeout {
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
                "upgrade refused: live work present; no binaries were replaced",
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
        std::thread::sleep(Duration::from_secs(1).min(timeout.saturating_sub(started.elapsed())));
    }
}
