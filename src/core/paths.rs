use crate::error::{Error, Result};
use std::env;
use std::path::PathBuf;

/// Base homeboy config directory (universal ~/.config/homeboy/ on all platforms)
pub fn homeboy() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = env::var("APPDATA").map_err(|_| {
            Error::internal_unexpected(
                "APPDATA environment variable not set on Windows".to_string(),
            )
        })?;
        Ok(PathBuf::from(appdata).join("homeboy"))
    }

    #[cfg(not(windows))]
    {
        let home = env::var("HOME").map_err(|_| {
            Error::internal_unexpected(
                "HOME environment variable not set on Unix-like system".to_string(),
            )
        })?;
        Ok(PathBuf::from(home).join(".config").join("homeboy"))
    }
}

/// Global homeboy.json config file path
pub fn homeboy_json() -> Result<PathBuf> {
    Ok(homeboy()?.join("homeboy.json"))
}

/// Projects directory
pub fn projects() -> Result<PathBuf> {
    Ok(homeboy()?.join("projects"))
}

/// Project directory path (e.g., ~/.config/homeboy/projects/{id}/)
pub fn project_dir(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id))
}

/// Project config file path (e.g., ~/.config/homeboy/projects/{id}/{id}.json)
pub fn project_config(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id).join(format!("{}.json", id)))
}

/// Servers directory
pub fn servers() -> Result<PathBuf> {
    Ok(homeboy()?.join("servers"))
}

/// Components directory
pub fn components() -> Result<PathBuf> {
    Ok(homeboy()?.join("components"))
}

/// Extensions directory
pub fn extensions() -> Result<PathBuf> {
    Ok(homeboy()?.join("extensions"))
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(homeboy()?.join("keys"))
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(homeboy()?.join("backups"))
}

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

/// Active rig run leases (~/.config/homeboy/rig-leases/).
pub fn rig_leases_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("rig-leases"))
}

/// Stacks directory (~/.config/homeboy/stacks/)
pub fn stacks() -> Result<PathBuf> {
    Ok(homeboy()?.join("stacks"))
}

/// Daemon runtime state directory (~/.config/homeboy/daemon/).
pub fn daemon_state_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("daemon"))
}

/// Daemon runtime state file (~/.config/homeboy/daemon/state.json).
pub fn daemon_state_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("state.json"))
}

/// Stack config file path (~/.config/homeboy/stacks/{id}.json)
pub fn stack_config(id: &str) -> Result<PathBuf> {
    Ok(stacks()?.join(format!("{}.json", id)))
}

/// Extension directory path
pub fn extension(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id))
}

/// Extension manifest file path
pub fn extension_manifest(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id).join(format!("{}.json", id)))
}

/// Key file path
pub fn key(server_id: &str) -> Result<PathBuf> {
    Ok(keys()?.join(format!("{}_id_rsa", server_id)))
}

/// Resolve path that may be absolute or relative to base.
pub fn resolve_path(base: &str, file: &str) -> PathBuf {
    if file.starts_with('/') {
        PathBuf::from(file)
    } else {
        PathBuf::from(base).join(file)
    }
}

/// Resolve path and return as String.
pub fn resolve_path_string(base: &str, file: &str) -> String {
    resolve_path(base, file).to_string_lossy().to_string()
}

pub(crate) fn resolve_optional_base_path(base_path: Option<&str>) -> Option<&str> {
    base_path.and_then(|value| (!value.trim().is_empty()).then_some(value.trim()))
}

pub fn join_remote_path(base_path: Option<&str>, path: &str) -> Result<String> {
    let path = path.trim();

    if path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Path cannot be empty",
            None,
            None,
        ));
    }

    if path.starts_with('/') {
        return Ok(path.to_string());
    }

    let Some(base) = resolve_optional_base_path(base_path) else {
        return Err(Error::config_missing_key("base_path", None));
    };

    if base.ends_with('/') {
        Ok(format!("{}{}", base, path))
    } else {
        Ok(format!("{}/{}", base, path))
    }
}

pub(crate) fn join_remote_child(base_path: Option<&str>, dir: &str, child: &str) -> Result<String> {
    let dir_path = join_remote_path(base_path, dir)?;
    let child = child.trim();

    if child.is_empty() {
        return Err(Error::validation_invalid_argument(
            "child",
            "Child path cannot be empty",
            None,
            None,
        ));
    }

    if dir_path.ends_with('/') {
        Ok(format!("{}{}", dir_path, child))
    } else {
        Ok(format!("{}/{}", dir_path, child))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_path_allows_absolute_paths_without_base() {
        assert_eq!(
            join_remote_path(None, "/var/log/syslog").unwrap(),
            "/var/log/syslog"
        );
    }

    #[test]
    fn join_remote_path_rejects_relative_paths_without_base() {
        assert!(join_remote_path(None, "file.json").is_err());
    }

    #[test]
    fn join_remote_path_joins_relative_paths() {
        assert_eq!(
            join_remote_path(Some("/var/www/site"), "file.json").unwrap(),
            "/var/www/site/file.json"
        );

        assert_eq!(
            join_remote_path(Some("/var/www/site/"), "file.json").unwrap(),
            "/var/www/site/file.json"
        );
    }

    #[test]
    fn join_remote_child_appends_child() {
        assert_eq!(
            join_remote_child(Some("/var/www/site"), "logs", "error.log").unwrap(),
            "/var/www/site/logs/error.log"
        );

        assert_eq!(
            join_remote_child(Some("/var/www/site"), "/var/log", "syslog").unwrap(),
            "/var/log/syslog"
        );
    }

    #[test]
    fn resolve_optional_base_path_trims_and_rejects_empty() {
        assert_eq!(
            resolve_optional_base_path(Some(" /var/www ")),
            Some("/var/www")
        );
        assert_eq!(resolve_optional_base_path(Some("   ")), None);
        assert_eq!(resolve_optional_base_path(None), None);
    }

    #[test]
    fn resolve_path_handles_relative() {
        let result = resolve_path_string("/base", "relative/path");
        assert_eq!(result, "/base/relative/path");
    }

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
}
