//! Runner-upgrade provider hook.
//!
//! The core upgrade flow (`upgrade::helpers`) upgrades configured runners as
//! part of `homeboy upgrade`. That orchestration is runner-domain — runner is an
//! optional Lab-offload feature — so core defines the [`RunnerUpgradeProvider`]
//! contract here and the runner layer registers an implementation at startup.
//!
//! With no provider registered there are no runners to upgrade, so the
//! [`NoopRunnerUpgradeProvider`] reports no upgrades and no build identity.

use std::path::Path;
use std::sync::Mutex;

use crate::upgrade::{ExtensionUpgradeEntry, InstallMethod, RunnerUpgradeEntry};
use homeboy_core::error::Result;

/// The runner-upgrade contract the core upgrade flow depends on. Implemented by
/// the runner layer and registered at startup.
pub trait RunnerUpgradeProvider: Send + Sync {
    /// Upgrade the configured runners from an explicit source checkout,
    /// returning `(upgraded, skipped)` entries.
    #[allow(clippy::too_many_arguments)]
    fn upgrade_configured_runners_with_explicit_source_path(
        &self,
        force: bool,
        method_override: Option<InstallMethod>,
        source_path: Option<&Path>,
        explicit_source_path: bool,
        expected_controller_identity: Option<&str>,
        runner_targets: &[String],
        extension_updates: &[ExtensionUpgradeEntry],
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)>;

    /// A short build-identity string for a source checkout (commit + dirty
    /// marker), or `None` if it can't be identified.
    fn source_checkout_build_identity(&self, source_path: &Path) -> Option<String>;
}

/// Default provider used when no runner layer is registered: no runners to
/// upgrade, no build identity.
struct NoopRunnerUpgradeProvider;

impl RunnerUpgradeProvider for NoopRunnerUpgradeProvider {
    fn upgrade_configured_runners_with_explicit_source_path(
        &self,
        _force: bool,
        _method_override: Option<InstallMethod>,
        _source_path: Option<&Path>,
        _explicit_source_path: bool,
        _expected_controller_identity: Option<&str>,
        _runner_targets: &[String],
        _extension_updates: &[ExtensionUpgradeEntry],
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
        Ok((Vec::new(), Vec::new()))
    }

    fn source_checkout_build_identity(&self, _source_path: &Path) -> Option<String> {
        None
    }
}

static PROVIDER: Mutex<Option<Box<dyn RunnerUpgradeProvider>>> = Mutex::new(None);

/// Register the runner-upgrade provider. Called once at startup by the runner
/// layer (via the CLI).
pub fn register_runner_upgrade_provider(provider: Box<dyn RunnerUpgradeProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Run `f` against the registered provider, or the no-op provider if none is
/// registered.
pub(crate) fn with_runner_upgrade<T>(f: impl FnOnce(&dyn RunnerUpgradeProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopRunnerUpgradeProvider),
    }
}
