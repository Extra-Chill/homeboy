use super::expand_tilde_path;
use std::env;
use std::path::{Path, PathBuf};

/// Environment variable selecting the root for rig-specific mutable state.
pub const RIG_REGISTRY_ROOT_ENV: &str = "HOMEBOY_RIG_REGISTRY_ROOT";

/// Root for rig-specific mutable state, defaulting to an already-resolved
/// config root.
///
/// `RIG_REGISTRY_ROOT_ENV` still outranks the supplied root, exactly as it
/// outranks `homeboy()` on the ambient path — the injected root replaces the
/// *default*, not the operator override.
pub fn rig_registry_root_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_from_env(env::var(RIG_REGISTRY_ROOT_ENV).ok(), config_root)
}

/// Resolve a rig registry root from an explicit environment value.
///
/// Kept pure so callers can verify the environment contract without mutating
/// process-global environment state.
pub fn rig_registry_root_from_env(value: Option<String>, default_root: &Path) -> PathBuf {
    value
        .filter(|path| !path.trim().is_empty())
        .map(expand_tilde_path)
        .unwrap_or_else(|| default_root.to_path_buf())
}

/// Rigs directory below an already-resolved config root.
pub fn rigs_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("rigs")
}

/// Rig config file path below an already-resolved config root.
pub fn rig_config_in_root(config_root: &Path, id: &str) -> PathBuf {
    rigs_in_root(config_root).join(format!("{}.json", id))
}

/// Installed rig package directory below an already-resolved config root.
pub fn rig_packages_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("rig-packages")
}

/// Cloned rig package path below an already-resolved config root.
pub fn rig_package_in_root(config_root: &Path, id: &str) -> PathBuf {
    rig_packages_in_root(config_root).join(id)
}

/// Rig source metadata directory below an already-resolved config root.
pub fn rig_sources_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("rig-sources")
}

/// Rig source metadata file below an already-resolved config root.
pub fn rig_source_metadata_in_root(config_root: &Path, id: &str) -> PathBuf {
    rig_sources_in_root(config_root).join(format!("{}.json", id))
}

/// Stack source metadata directory below an already-resolved config root.
pub fn stack_sources_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("stack-sources")
}

/// Stack source metadata file below an already-resolved config root.
pub fn stack_source_metadata_in_root(config_root: &Path, id: &str) -> PathBuf {
    stack_sources_in_root(config_root).join(format!("{}.json", id))
}

/// Rig state directory below an already-resolved config root.
pub fn rig_state_dir_in_root(config_root: &Path, id: &str) -> PathBuf {
    rigs_in_root(config_root).join(format!("{}.state", id))
}

/// Rig state file below an already-resolved config root.
pub fn rig_state_file_in_root(config_root: &Path, id: &str) -> PathBuf {
    rig_state_dir_in_root(config_root, id).join("state.json")
}

/// Rig service logs directory below an already-resolved config root.
pub fn rig_logs_dir_in_root(config_root: &Path, id: &str) -> PathBuf {
    rig_state_dir_in_root(config_root, id).join("logs")
}

/// Rig-owned baseline root below an already-resolved config root.
pub fn rig_baseline_root_in_root(config_root: &Path, id: &str) -> PathBuf {
    rig_state_dir_in_root(config_root, id).join("baselines")
}

/// Active rig run leases below an already-resolved config root.
pub fn rig_leases_dir_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("rig-leases")
}

/// Stacks directory below an already-resolved config root.
pub fn stacks_in_root(config_root: &Path) -> PathBuf {
    rig_registry_root_in_root(config_root).join("stacks")
}

/// Stack config file path below an already-resolved config root.
pub fn stack_config_in_root(config_root: &Path, id: &str) -> PathBuf {
    stacks_in_root(config_root).join(format!("{}.json", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test is the entry point for its own unit of work, so resolving once
    /// here is a boundary resolution (#7505).
    fn test_config_root() -> std::path::PathBuf {
        crate::homeboy().expect("config root")
    }

    #[test]
    fn test_rigs_path_under_homeboy_dir() {
        let path = rigs_in_root(&test_config_root());
        assert!(path.ends_with("rigs"), "got {}", path.display());
        assert!(path.parent().expect("parent").ends_with("homeboy"));
    }

    #[test]
    fn rig_registry_root_override_scopes_all_rig_state() {
        let default_root = PathBuf::from("/default/homeboy");
        assert_eq!(
            rig_registry_root_from_env(None, &default_root),
            default_root
        );
        assert_eq!(
            rig_registry_root_from_env(Some("   ".into()), &PathBuf::from("/default/homeboy")),
            PathBuf::from("/default/homeboy")
        );
        let root = rig_registry_root_from_env(
            Some("/runner/job-artifacts/rig-registry".into()),
            &PathBuf::from("/default/homeboy"),
        );
        assert_eq!(root, PathBuf::from("/runner/job-artifacts/rig-registry"));
        assert_eq!(
            root.join("rigs"),
            PathBuf::from("/runner/job-artifacts/rig-registry/rigs")
        );
        assert_eq!(
            root.join("rig-packages"),
            PathBuf::from("/runner/job-artifacts/rig-registry/rig-packages")
        );
        assert_eq!(
            root.join("rig-sources"),
            PathBuf::from("/runner/job-artifacts/rig-registry/rig-sources")
        );
        assert_eq!(
            root.join("stack-sources"),
            PathBuf::from("/runner/job-artifacts/rig-registry/stack-sources")
        );
        assert_eq!(
            root.join("rig-leases"),
            PathBuf::from("/runner/job-artifacts/rig-registry/rig-leases")
        );
        assert_eq!(
            root.join("stacks"),
            PathBuf::from("/runner/job-artifacts/rig-registry/stacks")
        );
    }

    #[test]
    fn test_rig_config_uses_id_filename() {
        let path = rig_config_in_root(&test_config_root(), "studio-dev");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("studio-dev.json")
        );
    }

    #[test]
    fn test_rig_state_dir_uses_state_suffix() {
        let path = rig_state_dir_in_root(&test_config_root(), "studio-dev");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }

    #[test]
    fn test_rig_state_file_nested_under_state_dir() {
        let path = rig_state_file_in_root(&test_config_root(), "studio-dev");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("state.json")
        );
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }

    #[test]
    fn test_rig_logs_dir_nested_under_state_dir() {
        let path = rig_logs_dir_in_root(&test_config_root(), "studio-dev");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("logs"));
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }

    #[test]
    fn test_rig_baseline_root_nested_under_state_dir() {
        let path = rig_baseline_root_in_root(&test_config_root(), "studio-dev");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("baselines"));
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }
}
