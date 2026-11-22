//! Runner-upgrade provider hook.
//!
//! The core upgrade flow (`upgrade::helpers`) upgrades configured runners as
//! part of `homeboy upgrade`. That orchestration is runner-domain — runner is an
//! optional Lab-offload feature — so core defines the [`RunnerUpgradeProvider`]
//! contract here and the runner layer registers an implementation at startup.
//!
//! With no provider registered there are no runners to upgrade, so the
//! [`NoopRunnerUpgradeProvider`] reports no upgrades.

use crate::upgrade::{
    ExtensionUpgradeEntry, RunnerCheckEntry, RunnerUpgradeEntry, ServiceRestartEntry,
};
use homeboy_core::error::Result;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Runner id to whether its release slot was already prefetched.
pub type RunnerReleasePrefetch = std::collections::BTreeMap<String, bool>;

/// The runner-upgrade contract the core upgrade flow depends on. Implemented by
/// the runner layer and registered at startup.
pub trait RunnerUpgradeProvider: Send + Sync {
    /// Read-only admission for selected runners before any controller,
    /// extension, or runner mutation. Implementations validate runner extension
    /// manifests against `candidate_version`.
    /// Returns one bounded failure entry per runner.
    fn preflight_configured_runners_for_upgrade(
        &self,
        runner_targets: &[String],
        candidate_version: &str,
    ) -> Result<Vec<RunnerUpgradeEntry>>;

    /// Acquire `tag` into each managed runner's immutable binary slot without
    /// selecting it, so the later runner refresh reuses the acquired slot.
    /// Reports, per runner, whether the slot was already acquired.
    fn acquire_runner_release(
        &self,
        _tag: &str,
        _runner_targets: &[String],
    ) -> Result<RunnerReleasePrefetch> {
        Ok(RunnerReleasePrefetch::new())
    }

    /// Refresh the configured runners into managed release slots,
    /// returning `(upgraded, skipped)` entries.
    fn upgrade_configured_runners(
        &self,
        expected_controller_identity: Option<&str>,
        runner_targets: &[String],
        extension_updates: &[ExtensionUpgradeEntry],
        promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)>;

    /// Read-only `upgrade --check` view of the selected runners: each one's
    /// selected binary version and runner-service binary versions against
    /// `target_version`. Must not mutate runner state.
    fn check_configured_runners(
        &self,
        _runner_targets: &[String],
        _target_version: &str,
    ) -> Result<Vec<RunnerCheckEntry>> {
        Ok(Vec::new())
    }

    /// Read-only view of the runner service hosted on this machine, when this
    /// machine is a runner host: each host unit that does not run
    /// `target_version`, with the command that converges it. That unit is
    /// owned by its controller's runner convergence, so a runner-host
    /// `homeboy upgrade` reports it pending instead of reporting success
    /// (#18550).
    fn host_runner_service_pending_restart(
        &self,
        _target_version: &str,
    ) -> Vec<ServiceRestartEntry> {
        Vec::new()
    }
}

/// Default provider used when no runner layer is registered: no runners to
/// upgrade.
struct NoopRunnerUpgradeProvider;

impl RunnerUpgradeProvider for NoopRunnerUpgradeProvider {
    fn preflight_configured_runners_for_upgrade(
        &self,
        _runner_targets: &[String],
        _candidate_version: &str,
    ) -> Result<Vec<RunnerUpgradeEntry>> {
        Ok(Vec::new())
    }

    fn upgrade_configured_runners(
        &self,
        _expected_controller_identity: Option<&str>,
        _runner_targets: &[String],
        _extension_updates: &[ExtensionUpgradeEntry],
        _promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
        Ok((Vec::new(), Vec::new()))
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerUpgradeProvider,
    noop: NoopRunnerUpgradeProvider,
    /// Register the runner-upgrade provider. Called once at startup by the runner
    /// layer (via the CLI).
    register: pub fn register_runner_upgrade_provider,
    /// Run `f` against the registered provider, or the no-op provider if none is
    /// registered.
    with: pub(crate) fn with_runner_upgrade,
}

/// Where configured-runner convergence currently is, sampled by the upgrade
/// heartbeat so `upgrade status` names the step it is blocked on instead of a
/// fixed label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerConvergenceProgress {
    pub runner_id: String,
    /// Convergence step: a `refresh-homeboy` sub-phase while the runner binary
    /// converges (`materialize`, `promote`, `reconnect`, ...), then
    /// `extension_sync` and `service_binaries`.
    pub sub_phase: String,
    /// The item within the step, e.g. the extension being synced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// What the step waits on, e.g. `Lab admission (pressure)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    /// Local run record of the runner job in flight (`homeboy runs show`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

type RunnerConvergenceProgressSampler = fn() -> Option<RunnerConvergenceProgress>;

static RUNNER_CONVERGENCE_PROGRESS: OnceLock<RunnerConvergenceProgressSampler> = OnceLock::new();

/// Register the runner layer's live convergence-progress sampler. Kept apart
/// from the provider registry: the heartbeat samples while
/// `upgrade_configured_runners` holds the provider slot.
pub fn register_runner_convergence_progress(sampler: RunnerConvergenceProgressSampler) {
    let _ = RUNNER_CONVERGENCE_PROGRESS.set(sampler);
}

pub(crate) fn runner_convergence_progress() -> Option<RunnerConvergenceProgress> {
    RUNNER_CONVERGENCE_PROGRESS
        .get()
        .and_then(|sample| sample())
}
