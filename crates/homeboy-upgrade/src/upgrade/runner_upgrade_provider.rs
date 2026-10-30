//! Runner-upgrade provider hook.
//!
//! The core upgrade flow (`upgrade::helpers`) upgrades configured runners as
//! part of `homeboy upgrade`. That orchestration is runner-domain — runner is an
//! optional Lab-offload feature — so core defines the [`RunnerUpgradeProvider`]
//! contract here and the runner layer registers an implementation at startup.
//!
//! With no provider registered there are no runners to upgrade, so the
//! [`NoopRunnerUpgradeProvider`] reports no upgrades.

use crate::upgrade::{ExtensionUpgradeEntry, RunnerCheckEntry, RunnerUpgradeEntry};
use homeboy_core::error::Result;

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
    fn acquire_runner_release(&self, _tag: &str, _runner_targets: &[String]) -> Result<()> {
        Ok(())
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
