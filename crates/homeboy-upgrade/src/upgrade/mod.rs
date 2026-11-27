mod admission;
mod constants;
mod execution;
pub mod extension_update_check;
mod github_api;
mod helpers;
mod operation;
mod planning;
pub mod release_catalog;
pub mod release_slot;
mod runner_admission_provider;
mod runner_upgrade_provider;
mod services;
mod types;
pub mod update_check;
mod validation;

pub use admission::{
    controller_upgrade_admission, register_controller_upgrade_admission_provider,
    ControllerUpgradeAdmission, ControllerUpgradeAdmissionProvider, ControllerUpgradeBlocker,
};
pub use execution::{controller_source_checkout, RELEASE_INSTALLER_SCRIPT};
pub use helpers::{
    current_build_version, current_version, detect_install_method, fetch_latest_version,
    run_upgrade_with_operation, start_installed_daemon,
};
pub use operation::{
    load_upgrade_operation_status, UpgradeOperationFailure, UpgradeOperationStatus,
    UpgradePromotionWaitStatus,
};
pub use planning::resolve_binary_on_path;
pub use release_catalog::{
    running_target_triple, InstallableSelection, ReleaseRef, SelectedRelease,
};
pub use runner_admission_provider::{
    register_runner_direct_active_jobs_provider, runner_direct_active_job_count,
    RunnerDirectActiveJobsProvider,
};
pub use runner_upgrade_provider::{
    register_runner_convergence_progress, register_runner_upgrade_provider,
    RunnerConvergenceProgress, RunnerReleasePrefetch, RunnerUpgradeProvider,
};
pub(crate) use runner_upgrade_provider::{runner_convergence_progress, with_runner_upgrade};
pub use services::restart_extension_services;
pub use types::{
    DaemonPendingRestart, ExtensionPreflightBlocker, ExtensionUpgradeEntry, ExtensionUpgradeSkip,
    HomeboyPathBinarySync, InstallMethod, ReleasePrefetch, RunnerCheckEntry,
    RunnerConvergenceDisposition, RunnerDaemonDriftEntry, RunnerExtensionSyncEntry,
    RunnerServiceBinaryEntry, RunnerUpgradeEntry, ServiceRestartEntry, UpgradeComponentStatus,
    UpgradePreflight, UpgradeResult, VersionCheck,
};
pub use validation::{check_for_updates, check_for_updates_with_runners};

impl InstallMethod {
    pub fn as_str(&self) -> String {
        match self {
            InstallMethod::Homebrew => "homebrew",
            InstallMethod::Source => "source",
            InstallMethod::Binary => "binary",
            InstallMethod::Unknown => "unknown",
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detect_install_method_prefers_exe_path_and_uses_brew_list_as_fallback() {
        let brew_lists_homeboy =
            |cmd: &str, args: &[&str]| cmd == "brew" && args == ["list", "homeboy"];
        for (exe, brew_listed, expected) in [
            (
                "/Users/user/.cargo/bin/homeboy",
                true,
                InstallMethod::Binary,
            ),
            ("/opt/homebrew/bin/homeboy", false, InstallMethod::Homebrew),
            (
                "/Applications/Homeboy/homeboy",
                true,
                InstallMethod::Homebrew,
            ),
        ] {
            let method = helpers::detect_install_method_from_exe_path(exe, |cmd, args| {
                brew_listed && brew_lists_homeboy(cmd, args)
            });
            assert_eq!(method, expected, "{exe}");
        }
    }

    #[test]
    fn resolve_binary_on_path_var_finds_first_existing_binary() {
        let base = tempdir().unwrap();
        let first = base.path().join("first");
        let second = base.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let path_var = format!("{}:{}", first.display(), second.display());

        assert!(planning::resolve_binary_on_path_var(&path_var).is_none());

        fs::write(second.join("homeboy"), "#!/bin/sh\n").unwrap();
        let found = planning::resolve_binary_on_path_var(&path_var).unwrap();
        assert_eq!(found, second.join("homeboy"));
    }
}
