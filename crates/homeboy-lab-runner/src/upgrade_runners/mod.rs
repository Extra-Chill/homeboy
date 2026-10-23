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
    fn drain_targets(&self, targets: &[String]) -> Result<Vec<String>> {
        Ok(runner_upgrade_targets(targets)?
            .into_iter()
            .filter(|runner| runner.kind == homeboy_runner_contract::RunnerKind::Ssh)
            .map(|runner| runner.id)
            .collect())
    }

    fn set_upgrade_drain(
        &self,
        targets: &[String],
        operation: &str,
        runs: &[String],
        resume: bool,
    ) -> Result<()> {
        for runner in targets {
            let session = crate::status(runner)?.session.ok_or_else(|| {
                homeboy_core::Error::internal_unexpected(format!(
                    "runner {runner} has no connected drain endpoint"
                ))
            })?;
            crate::execution::daemon_api_post_json_for_session_with_timeout(
                &session,
                "/upgrade/drain",
                &serde_json::json!({"operation_id": operation, "runs": runs, "resume": resume, "timeout_ms": 86_400_000}),
                None,
                std::time::Duration::from_secs(10),
            )?;
        }
        Ok(())
    }

    fn verify_upgrade_admission(&self, targets: &[String]) -> Result<()> {
        for runner in targets {
            crate::homeboy_refresh::probe_reconnected_admission_readiness(
                runner,
                "upgraded runner",
            )?;
            let report = crate::status(runner)?;
            if !report.admission_summary(0).accepting_jobs {
                return Err(homeboy_core::Error::internal_unexpected(format!(
                    "runner {runner} is not accepting jobs after upgrade"
                ))
                .with_hint(format!(
                    "homeboy runner refresh-homeboy {runner} --reconnect"
                )));
            }
        }
        Ok(())
    }

    fn drain_jobs(&self, targets: &[String]) -> Result<Vec<(String, String)>> {
        let mut jobs = Vec::new();
        for runner in targets {
            let response = crate::daemon_api_get(runner, "/jobs")?;
            let body = crate::execution::canonical_daemon_body(&response, "upgrade drain jobs")?;
            let entries = body["jobs"].as_array().ok_or_else(|| {
                homeboy_core::Error::internal_unexpected(format!(
                    "runner {runner} returned no authoritative job inventory"
                ))
            })?;
            for job in entries {
                if matches!(job["status"].as_str(), Some("queued" | "running")) {
                    let id = job["id"].as_str().ok_or_else(|| {
                        homeboy_core::Error::internal_unexpected("runner job identity missing")
                    })?;
                    jobs.push((runner.clone(), id.to_string()));
                }
            }
            // Older generations remain owners until settlement. Do not upgrade
            // while a retained generation still owns any job identity.
            for owner in crate::generation_store::status_job_owners(runner)? {
                for job in owner.job_ids {
                    if !jobs
                        .iter()
                        .any(|(id, existing)| id == runner && existing == &job)
                    {
                        jobs.push((runner.clone(), job));
                    }
                }
            }
        }
        Ok(jobs)
    }

    fn cancel_drain_job(&self, runner: &str, job: &str) -> Result<()> {
        crate::execution::daemon_api_post(runner, &format!("/jobs/{job}/cancel"))?;
        Ok(())
    }

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
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
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
