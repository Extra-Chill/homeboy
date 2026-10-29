//! Runner upgrade orchestration, split into focused submodules.
//!
//! This module coordinates upgrading configured runners: driving the upgrade
//! executor chain, recovering from failed upgrades, realigning drifted
//! `homeboy_path` configuration, syncing runner extensions, probing runner
//! versions, building remediation commands, and rendering upgrade reports.
//!
//! The submodules are a mechanical split of the former single-file module; all
//! items are re-exported here so existing `homeboy_upgrade::upgrade::runners::*`
//! paths continue to resolve unchanged.

mod commands;
mod extensions;
mod failure;
mod orchestration;
mod path_alignment;
mod reporting;
mod service_binaries;
mod source_checkout;
mod version;

pub(super) use commands::*;
pub(super) use extensions::*;
pub(super) use failure::*;
pub(super) use orchestration::*;
pub(super) use path_alignment::*;
pub(super) use reporting::*;
pub(super) use service_binaries::*;
pub(super) use source_checkout::*;
pub(super) use version::*;

use std::path::Path;

use homeboy_core::error::Result;
use homeboy_upgrade::upgrade::{
    ExtensionUpgradeEntry, InstallMethod, RunnerCheckEntry, RunnerUpgradeEntry,
    RunnerUpgradeProvider,
};

/// The runner layer's `RunnerUpgradeProvider`, delegating to this cluster's
/// upgrade orchestration. Registered with core at startup.
pub struct RunnerUpgrade;

struct RunnerDirectActiveJobs;

impl homeboy_upgrade::upgrade::RunnerDirectActiveJobsProvider for RunnerDirectActiveJobs {
    fn direct_active_job_count(&self, runner_id: &str) -> Option<usize> {
        crate::runner_admission_snapshot(runner_id)
            .ok()?
            .summary
            .live_daemon_job_count
    }
}

impl RunnerUpgradeProvider for RunnerUpgrade {
    fn preflight_configured_runners_for_upgrade(
        &self,
        method_override: Option<InstallMethod>,
        source_path: Option<&Path>,
        explicit_source_path: bool,
        runner_targets: &[String],
        candidate_version: &str,
    ) -> Result<Vec<RunnerUpgradeEntry>> {
        preflight_configured_runners_for_upgrade(
            method_override,
            source_path,
            explicit_source_path,
            runner_targets,
            candidate_version,
        )
    }

    fn acquire_runner_release(
        &self,
        tag: &str,
        targets: &[String],
    ) -> Result<Vec<homeboy_upgrade::upgrade::AcquiredRunnerRelease>> {
        let runners = orchestration::runner_upgrade_targets(targets)?;
        std::thread::scope(|scope| {
            let jobs: Vec<_> = runners
                .iter()
                .map(|runner| {
                    scope.spawn(move || {
                        let started_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .expect("clock after epoch")
                            .as_millis() as u64;
                        let started = std::time::Instant::now();
                        crate::homeboy_refresh::acquire_release_slot(&runner.id, tag).map(
                            |binary_path| homeboy_upgrade::upgrade::AcquiredRunnerRelease {
                                runner_id: runner.id.clone(),
                                binary_path,
                                started_at,
                                duration_ms: started.elapsed().as_millis() as u64,
                            },
                        )
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().expect("runner acquisition panicked"))
                .collect()
        })
    }

    fn upgrade_configured_runners_with_explicit_source_path(
        &self,
        force: bool,
        method_override: Option<InstallMethod>,
        source_path: Option<&Path>,
        explicit_source_path: bool,
        expected_controller_identity: Option<&str>,
        runner_targets: &[String],
        extension_updates: &[ExtensionUpgradeEntry],
        promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
        acquired: &[homeboy_upgrade::upgrade::AcquiredRunnerRelease],
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
        if !acquired.is_empty() {
            let mut updated = Vec::new();
            let mut skipped = Vec::new();
            for release in acquired {
                let runner = crate::load(&release.runner_id)?;
                if let Ok(report) = crate::status(&runner.id) {
                    let live_jobs = report.active_job_count.max(report.active_jobs.len());
                    if live_jobs > 0 {
                        let mut entry = failure::runner_upgrade_failure_entry(
                            &runner.id,
                            runner.settings.homeboy_path.clone().unwrap_or_default(),
                            None,
                            0,
                            format!("runner busy: {live_jobs} live job(s)"),
                        );
                        entry.success = true;
                        entry.deferred = true;
                        entry.recovery_commands.clear();
                        updated.push(entry);
                        continue;
                    }
                }
                let mut entry = orchestration::refresh_managed_immutable_runner(
                    &runner,
                    runner.settings.homeboy_path.clone().unwrap_or_default(),
                    version::runner_homeboy_version(
                        &runner,
                        runner.settings.homeboy_path.as_deref().unwrap_or("homeboy"),
                        &mut crate::exec,
                    )
                    .ok()
                    .flatten(),
                    None,
                    expected_controller_identity,
                    extension_updates,
                    &mut crate::exec,
                    Some(&release.binary_path),
                );
                entry.phases["binary"] = serde_json::json!({
                    "started_at": release.started_at, "duration_ms": release.duration_ms,
                });
                if entry.success {
                    updated.push(entry);
                } else {
                    skipped.push(entry);
                }
            }
            return Ok(service_binaries::converge_service_binaries_for_entries(
                &orchestration::runner_upgrade_targets(runner_targets)?,
                updated,
                skipped,
            ));
        }
        upgrade_configured_runners_with_explicit_source_path(
            force,
            method_override,
            source_path,
            explicit_source_path,
            expected_controller_identity,
            runner_targets,
            extension_updates,
            promotion_lease,
        )
    }

    fn source_checkout_build_identity(&self, source_path: &Path) -> Option<String> {
        source_checkout_build_identity(source_path)
    }

    fn check_configured_runners(
        &self,
        runner_targets: &[String],
        target_version: &str,
    ) -> Result<Vec<RunnerCheckEntry>> {
        check_configured_runners(runner_targets, target_version)
    }
}

/// Register this cluster's runner-upgrade provider with core. Called once at
/// startup.
pub fn register() {
    homeboy_upgrade::upgrade::register_runner_upgrade_provider(Box::new(RunnerUpgrade));
    homeboy_upgrade::upgrade::register_runner_direct_active_jobs_provider(Box::new(
        RunnerDirectActiveJobs,
    ));
}

#[cfg(test)]
mod tests;
