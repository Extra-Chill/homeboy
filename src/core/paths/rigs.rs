use super::{homeboy, Result};
use std::path::PathBuf;

/// Rigs directory (~/.config/homeboy/rigs/)
pub fn rigs() -> Result<PathBuf> {
    Ok(homeboy()?.join("rigs"))
}

/// Rig config file path (~/.config/homeboy/rigs/{id}.json)
pub fn rig_config(id: &str) -> Result<PathBuf> {
    Ok(rigs()?.join(format!("{}.json", id)))
}

/// Installed rig package directory (~/.config/homeboy/rig-packages/)
pub fn rig_packages() -> Result<PathBuf> {
    Ok(homeboy()?.join("rig-packages"))
}

/// Cloned rig package path (~/.config/homeboy/rig-packages/{id}/)
pub fn rig_package(id: &str) -> Result<PathBuf> {
    Ok(rig_packages()?.join(id))
}

/// Rig source metadata directory (~/.config/homeboy/rig-sources/)
pub fn rig_sources() -> Result<PathBuf> {
    Ok(homeboy()?.join("rig-sources"))
}

/// Rig source metadata file (~/.config/homeboy/rig-sources/{id}.json)
pub fn rig_source_metadata(id: &str) -> Result<PathBuf> {
    Ok(rig_sources()?.join(format!("{}.json", id)))
}

/// Stack source metadata directory (~/.config/homeboy/stack-sources/)
pub fn stack_sources() -> Result<PathBuf> {
    Ok(homeboy()?.join("stack-sources"))
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
    Ok(homeboy()?.join("rig-leases"))
}

/// Stacks directory (~/.config/homeboy/stacks/)
pub fn stacks() -> Result<PathBuf> {
    Ok(homeboy()?.join("stacks"))
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
