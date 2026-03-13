mod types;
mod constants;
mod helpers;
mod planning;
mod execution;
mod validation;
pub mod update_check;

pub use types::*;
pub use helpers::{
    current_version, detect_install_method, fetch_latest_version,
    restart_with_new_binary, run_upgrade_with_method,
};
pub use planning::resolve_binary_on_path;
pub use validation::check_for_updates;

impl InstallMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallMethod::Homebrew => "homebrew",
            InstallMethod::Cargo => "cargo",
            InstallMethod::Source => "source",
            InstallMethod::Binary => "binary",
            InstallMethod::Unknown => "unknown",
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
    fn test_current_version() {
        let version = current_version();
        assert!(!version.is_empty());
        assert!(version.contains('.'));
    }

    #[test]
    fn test_install_method_strings() {
        assert_eq!(InstallMethod::Homebrew.as_str(), "homebrew");
        assert_eq!(InstallMethod::Cargo.as_str(), "cargo");
        assert_eq!(InstallMethod::Source.as_str(), "source");
        assert_eq!(InstallMethod::Binary.as_str(), "binary");
        assert_eq!(InstallMethod::Unknown.as_str(), "unknown");
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
