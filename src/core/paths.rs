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
}
