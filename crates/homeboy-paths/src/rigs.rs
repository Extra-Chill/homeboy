use super::{expand_tilde_path, homeboy, Result};
use std::env;
use std::path::PathBuf;

/// Environment variable selecting the root for rig-specific mutable state.
pub const RIG_REGISTRY_ROOT_ENV: &str = "HOMEBOY_RIG_REGISTRY_ROOT";

/// Root for rig-specific mutable state.
///
/// An unset or blank override retains the historical `homeboy()` root. This
/// intentionally scopes only rig registries; general Homeboy configuration is
/// unaffected.
pub fn rig_registry_root() -> Result<PathBuf> {
    let default_root = homeboy()?;
    Ok(rig_registry_root_from_env(
        env::var(RIG_REGISTRY_ROOT_ENV).ok(),
        &default_root,
    ))
}

/// Resolve a rig registry root from an explicit environment value.
///
/// Kept pure so callers can verify the environment contract without mutating
/// process-global environment state.
pub fn rig_registry_root_from_env(value: Option<String>, default_root: &PathBuf) -> PathBuf {
    value
        .filter(|path| !path.trim().is_empty())
        .map(expand_tilde_path)
        .unwrap_or_else(|| default_root.clone())
}

/// Rigs directory (~/.config/homeboy/rigs/)
pub fn rigs() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("rigs"))
}

/// Rig config file path (~/.config/homeboy/rigs/{id}.json)
pub fn rig_config(id: &str) -> Result<PathBuf> {
    Ok(rigs()?.join(format!("{}.json", id)))
}

/// Installed rig package directory (~/.config/homeboy/rig-packages/)
pub fn rig_packages() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("rig-packages"))
}

/// Cloned rig package path (~/.config/homeboy/rig-packages/{id}/)
pub fn rig_package(id: &str) -> Result<PathBuf> {
    Ok(rig_packages()?.join(id))
}

/// Rig source metadata directory (~/.config/homeboy/rig-sources/)
pub fn rig_sources() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("rig-sources"))
}

/// Rig source metadata file (~/.config/homeboy/rig-sources/{id}.json)
pub fn rig_source_metadata(id: &str) -> Result<PathBuf> {
    Ok(rig_sources()?.join(format!("{}.json", id)))
}

/// Stack source metadata directory (~/.config/homeboy/stack-sources/)
pub fn stack_sources() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("stack-sources"))
}

/// Stack source metadata file (~/.config/homeboy/stack-sources/{id}.json)
pub fn stack_source_metadata(id: &str) -> Result<PathBuf> {
    Ok(stack_sources()?.join(format!("{}.json", id)))
}

/// Rig state directory (~/.config/homeboy/rigs/{id}.state/)
/// Holds service PIDs, logs, and last check results.
pub fn rig_state_dir(id: &str) -> Result<PathBuf> {
    Ok(rigs()?.join(format!("{}.state", id)))
}

/// Rig state file (~/.config/homeboy/rigs/{id}.state/state.json)
pub fn rig_state_file(id: &str) -> Result<PathBuf> {
    Ok(rig_state_dir(id)?.join("state.json"))
}

/// Rig service logs directory (~/.config/homeboy/rigs/{id}.state/logs/)
pub fn rig_logs_dir(id: &str) -> Result<PathBuf> {
    Ok(rig_state_dir(id)?.join("logs"))
}

/// Rig-owned baseline root (~/.config/homeboy/rigs/{id}.state/baselines/).
///
/// Baselines for rig-owned workloads (e.g. trace --rig) live here instead of
/// in the target component checkout. The component repo may be unrelated to
/// Homeboy and must stay clean.
pub fn rig_baseline_root(id: &str) -> Result<PathBuf> {
    Ok(rig_state_dir(id)?.join("baselines"))
}

/// Active rig run leases (~/.config/homeboy/rig-leases/).
pub fn rig_leases_dir() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("rig-leases"))
}

/// Stacks directory (~/.config/homeboy/stacks/)
pub fn stacks() -> Result<PathBuf> {
    Ok(rig_registry_root()?.join("stacks"))
}

/// Stack config file path (~/.config/homeboy/stacks/{id}.json)
pub fn stack_config(id: &str) -> Result<PathBuf> {
    Ok(stacks()?.join(format!("{}.json", id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rigs_path_under_homeboy_dir() {
        let path = rigs().expect("rigs path resolves");
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
        let path = rig_config("studio-dev").expect("rig_config resolves");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("studio-dev.json")
        );
    }

    #[test]
    fn test_rig_state_dir_uses_state_suffix() {
        let path = rig_state_dir("studio-dev").expect("rig_state_dir resolves");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }

    #[test]
    fn test_rig_state_file_nested_under_state_dir() {
        let path = rig_state_file("studio-dev").expect("rig_state_file resolves");
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
        let path = rig_logs_dir("studio-dev").expect("rig_logs_dir resolves");
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
        let path = rig_baseline_root("studio-dev").expect("rig_baseline_root resolves");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("baselines"));
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("studio-dev.state")
        );
    }
}
