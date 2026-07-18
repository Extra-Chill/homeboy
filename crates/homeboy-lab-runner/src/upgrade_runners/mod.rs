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
mod source_checkout;
mod version;

pub(super) use commands::*;
pub(super) use extensions::*;
pub(super) use failure::*;
pub(super) use orchestration::*;
pub(super) use path_alignment::*;
pub(super) use reporting::*;
pub(super) use source_checkout::*;
pub(super) use version::*;

use std::path::Path;

use homeboy_core::error::Result;
use homeboy_upgrade::upgrade::{
    ExtensionUpgradeEntry, InstallMethod, RunnerUpgradeEntry, RunnerUpgradeProvider,
};

/// The runner layer's `RunnerUpgradeProvider`, delegating to this cluster's
/// upgrade orchestration. Registered with core at startup.
pub struct RunnerUpgrade;

impl RunnerUpgradeProvider for RunnerUpgrade {
    fn upgrade_configured_runners_with_explicit_source_path(
        &self,
        force: bool,
        method_override: Option<InstallMethod>,
        source_path: Option<&Path>,
        explicit_source_path: bool,
        runner_targets: &[String],
        extension_updates: &[ExtensionUpgradeEntry],
    ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
        upgrade_configured_runners_with_explicit_source_path(
            force,
            method_override,
            source_path,
            explicit_source_path,
            runner_targets,
            extension_updates,
        )
    }

    fn source_checkout_build_identity(&self, source_path: &Path) -> Option<String> {
        source_checkout_build_identity(source_path)
    }
}

/// Register this cluster's runner-upgrade provider with core. Called once at
/// startup.
pub fn register() {
    homeboy_upgrade::upgrade::register_runner_upgrade_provider(Box::new(RunnerUpgrade));
}

#[cfg(test)]
mod tests;
