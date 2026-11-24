//! Runner upgrade orchestration, split into focused submodules.
//!
//! Every SSH runner's binary converges through the same decision
//! `runner connect` makes (`connection::binary_convergence`), aimed at the
//! controller the upgrade installed. Only a converged or already-current
//! binary is followed by extension sync and service-binary convergence.

mod commands;
mod extensions;
mod failure;
mod orchestration;
pub(crate) mod progress;
mod reporting;
mod service_binaries;
mod version;

pub(super) use commands::*;
pub(super) use extensions::*;
pub(super) use failure::*;
pub(super) use orchestration::*;
pub(super) use reporting::*;
pub(super) use service_binaries::*;
pub(super) use version::*;

use homeboy_core::error::Result;
use homeboy_upgrade::upgrade::{
    ExtensionUpgradeEntry, RunnerCheckEntry, RunnerUpgradeEntry, RunnerUpgradeProvider,
};

/// The runner layer's `RunnerUpgradeProvider`, delegating to this cluster's
/// upgrade orchestration. Registered with core at startup.
pub struct RunnerUpgrade;

struct RunnerDirectActiveJobs;

impl homeboy_upgrade::upgrade::RunnerDirectActiveJobsProvider for RunnerDirectActiveJobs {
    fn direct_active_job_count(&self, runner_id: &str) -> Option<usize> {
        crate::runner_admission_snapshot(
            runner_id,
            None,
            crate::RunnerAdmissionReadiness::Observation,
        )
        .ok()?
        .summary
        .live_daemon_job_count
    }
}

impl RunnerUpgradeProvider for RunnerUpgrade {
    fn preflight_configured_runners_for_upgrade(
        &self,
        runner_targets: &[String],
        candidate_version: &str,
    ) -> Result<Vec<RunnerUpgradeEntry>> {
        preflight_configured_runners_for_upgrade(runner_targets, candidate_version)
    }

    fn acquire_runner_release(
        &self,
        tag: &str,
        runner_targets: &[String],
    ) -> Result<homeboy_upgrade::RunnerReleasePrefetch> {
        let runners: Vec<_> = runner_upgrade_targets(runner_targets)?
            .into_iter()
            .collect();
        std::thread::scope(|scope| {
            runners
                .iter()
                .map(|runner| {
                    scope.spawn(|| {
                        crate::homeboy_refresh::acquire_release_slot(&runner.id, tag)
                            .map(|prefetched| (runner.id.clone(), prefetched))
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|job| job.join().expect("runner release acquisition panicked"))
                .collect()
        })
    }

    fn upgrade_configured_runners(
        &self,
        expected_controller_identity: Option<&str>,
        runner_targets: &[String],
        extension_updates: &[ExtensionUpgradeEntry],
        promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
        upgrade_configured_runners(
            expected_controller_identity,
            runner_targets,
            extension_updates,
            promotion_lease,
        )
    }

    fn check_configured_runners(
        &self,
        runner_targets: &[String],
        target_version: &str,
    ) -> Result<Vec<RunnerCheckEntry>> {
        check_configured_runners(runner_targets, target_version)
    }

    fn host_runner_service_pending_restart(
        &self,
        target_version: &str,
    ) -> Vec<homeboy_upgrade::upgrade::ServiceRestartEntry> {
        service_binaries::host_service_pending_restart(
            &crate::connection::observe_local_service_units(),
            target_version,
        )
    }
}

/// Register this cluster's runner-upgrade provider with core. Called once at
/// startup.
pub fn register() {
    homeboy_upgrade::upgrade::register_runner_upgrade_provider(Box::new(RunnerUpgrade));
    homeboy_upgrade::upgrade::register_runner_convergence_progress(progress::sample);
    homeboy_upgrade::upgrade::register_runner_direct_active_jobs_provider(Box::new(
        RunnerDirectActiveJobs,
    ));
}

#[cfg(test)]
mod tests;
