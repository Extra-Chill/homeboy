mod constants;
mod execution;
mod helpers;
mod planning;
mod runners;
mod services;
mod types;
pub mod update_check;
mod validation;

pub(crate) use constants::VERSION;
pub use helpers::{
    current_build_version, current_version, detect_install_method, fetch_latest_version,
    restart_with_new_binary, run_upgrade_with_method,
};
pub use planning::resolve_binary_on_path;
pub use types::{
    ExtensionUpgradeEntry, InstallMethod, RunnerUpgradeEntry, ServiceRestartEntry, UpgradeResult,
    VersionCheck,
};
pub use validation::check_for_updates;

impl InstallMethod {
    pub fn as_str(&self) -> String {
        if *self == InstallMethod::Homebrew {
            "homebrew".to_string()
        } else if *self == InstallMethod::Source {
            "source".to_string()
        } else if *self == InstallMethod::Binary {
            "binary".to_string()
        } else if *self == InstallMethod::Unknown {
            "unknown".to_string()
        } else {
            crate::core::defaults::secondary_install_method_key()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_version_comparison() {
        assert!(helpers::version_is_newer("0.12.0", "0.11.0"));
        assert!(helpers::version_is_newer("1.0.0", "0.99.99"));
        assert!(helpers::version_is_newer("0.11.1", "0.11.0"));
        assert!(!helpers::version_is_newer("0.11.0", "0.11.0"));
        assert!(!helpers::version_is_newer("0.10.0", "0.11.0"));
    }

    #[test]
    fn same_version_upgrade_completion_still_syncs_extensions_and_runners() {
        assert!(helpers::should_sync_after_upgrade(Some("0.228.5")));
        assert!(!helpers::should_sync_after_upgrade(None));
    }

    #[test]
    fn test_current_version() {
        let version = current_version();
        assert!(!version.is_empty());
        assert!(version.contains('.'));
    }

    #[test]
    fn test_install_method_strings() {
        assert_eq!(InstallMethod::Homebrew.as_str(), "homebrew");
        assert_eq!(
            InstallMethod::Secondary.as_str(),
            crate::core::defaults::secondary_install_method_key()
        );
        assert_eq!(InstallMethod::Source.as_str(), "source");
        assert_eq!(InstallMethod::Binary.as_str(), "binary");
        assert_eq!(InstallMethod::Unknown.as_str(), "unknown");
    }

    #[test]
    fn test_detect_install_method_prefers_active_secondary_path_over_brew_list() {
        let secondary_path = format!(
            "/Users/user/.{}/bin/homeboy",
            crate::core::defaults::secondary_install_method_key()
        );
        let method = helpers::detect_install_method_from_exe_path(&secondary_path, |cmd, args| {
            cmd == "brew" && args == ["list", "homeboy"]
        });

        assert_eq!(method, InstallMethod::Secondary);
    }

    #[test]
    fn test_detect_install_method_preserves_active_homebrew_path() {
        let method = helpers::detect_install_method_from_exe_path(
            "/opt/homebrew/bin/homeboy",
            |_cmd, _args| false,
        );

        assert_eq!(method, InstallMethod::Homebrew);
    }

    #[test]
    fn test_detect_install_method_uses_brew_list_only_as_fallback() {
        let method = helpers::detect_install_method_from_exe_path(
            "/Applications/Homeboy/homeboy",
            |cmd, args| cmd == "brew" && args == ["list", "homeboy"],
        );

        assert_eq!(method, InstallMethod::Homebrew);
    }

    #[test]
    fn test_resolve_binary_on_path_var_finds_first_existing_binary() {
        let base = tempdir().unwrap();
        let first = base.path().join("first");
        let second = base.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(second.join("homeboy"), "#!/bin/sh\n").unwrap();

        let path_var = format!("{}:{}", first.display(), second.display());
        let found = planning::resolve_binary_on_path_var(&path_var).unwrap();
        assert_eq!(found, second.join("homeboy"));
    }

    #[test]
    fn test_resolve_binary_on_path_var_returns_none_when_missing() {
        let base = tempdir().unwrap();
        let first = base.path().join("first");
        let second = base.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let path_var = format!("{}:{}", first.display(), second.display());
        let found = planning::resolve_binary_on_path_var(&path_var);
        assert!(found.is_none());
    }
}
