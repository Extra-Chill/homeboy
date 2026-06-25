use crate::core::error::{Error, Result};
use std::env;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

mod rigs;

pub use rigs::*;

fn artifact_root_override() -> &'static Mutex<Option<PathBuf>> {
    static OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Set a process-local artifact root override.
///
/// This is intentionally process-scoped so CLI flags can outrank environment
/// and config without mutating global config or environment variables.
pub fn set_artifact_root_override(path: Option<PathBuf>) {
    let mut guard = artifact_root_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = path;
}

/// Base product config directory (universal ~/.config/homeboy/ on all platforms)
pub fn homeboy() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = env::var("APPDATA").map_err(|_| {
            Error::internal_unexpected(
                "APPDATA environment variable not set on Windows".to_string(),
            )
        })?;
        Ok(PathBuf::from(appdata)
            .join(crate::core::product_identity::PRODUCT_IDENTITY.config_dirname))
    }

    #[cfg(not(windows))]
    {
        let home = env::var("HOME").map_err(|_| {
            Error::internal_unexpected(
                "HOME environment variable not set on Unix-like system".to_string(),
            )
        })?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join(crate::core::product_identity::PRODUCT_IDENTITY.config_dirname))
    }
}

/// Global product config file path
pub fn homeboy_json() -> Result<PathBuf> {
    Ok(crate::core::product_identity::PRODUCT_IDENTITY.config_file(homeboy()?))
}

/// Base Homeboy data directory for local observed state.
///
/// Config/spec files remain under `homeboy()` (`~/.config/homeboy`). This
/// directory is for machine-local observations such as the SQLite store.
pub fn homeboy_data() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = env::var("LOCALAPPDATA")
            .or_else(|_| env::var("APPDATA"))
            .map_err(|_| {
                Error::internal_unexpected(
                    "LOCALAPPDATA or APPDATA environment variable not set on Windows".to_string(),
                )
            })?;
        Ok(PathBuf::from(base).join(crate::core::product_identity::PRODUCT_IDENTITY.data_dirname))
    }

    #[cfg(not(windows))]
    {
        if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
            if !xdg_data_home.trim().is_empty() {
                return Ok(PathBuf::from(xdg_data_home)
                    .join(crate::core::product_identity::PRODUCT_IDENTITY.data_dirname));
            }
        }

        let home = env::var("HOME").map_err(|_| {
            Error::internal_unexpected(
                "HOME environment variable not set on Unix-like system".to_string(),
            )
        })?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(crate::core::product_identity::PRODUCT_IDENTITY.data_dirname))
    }
}

/// Local SQLite observation-store path.
pub fn observation_db() -> Result<PathBuf> {
    Ok(homeboy_data()?.join("homeboy.sqlite"))
}

/// Root directory for copied run artifacts.
///
/// Precedence:
/// 1. process-local CLI override (`homeboy --artifact-root <path>`)
/// 2. `HOMEBOY_ARTIFACT_ROOT`
/// 3. global config `/artifact_root`
/// 4. historical default: `<homeboy_data>/artifacts`
pub fn artifact_root() -> Result<PathBuf> {
    if let Some(path) = artifact_root_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(expand_path(path));
    }

    if let Ok(path) = env::var("HOMEBOY_ARTIFACT_ROOT") {
        if !path.trim().is_empty() {
            return Ok(expand_path(PathBuf::from(path)));
        }
    }

    if let Some(path) = crate::core::defaults::load_config().artifact_root {
        if !path.trim().is_empty() {
            return Ok(expand_path(PathBuf::from(path)));
        }
    }

    Ok(homeboy_data()?.join("artifacts"))
}

fn expand_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    PathBuf::from(shellexpand::tilde(&raw).into_owned())
}

pub(crate) fn sanitize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
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

/// First-class agent runtime directory.
pub fn ai_runtimes() -> Result<PathBuf> {
    Ok(homeboy()?.join("agent-runtimes"))
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(homeboy()?.join("keys"))
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(homeboy()?.join("backups"))
}

/// Daemon runtime state directory (~/.config/homeboy/daemon/).
fn daemon_state_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("daemon"))
}

/// Daemon runtime state file (~/.config/homeboy/daemon/state.json).
pub fn daemon_state_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("state.json"))
}

/// Daemon durable job state file (~/.config/homeboy/daemon/jobs.json).
pub fn daemon_jobs_file() -> Result<PathBuf> {
    Ok(daemon_state_dir()?.join("jobs.json"))
}

/// Runner connection session state directory (~/.config/homeboy/runner-sessions/).
fn runner_sessions_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("runner-sessions"))
}

/// Runner connection session state file (~/.config/homeboy/runner-sessions/{id}.json).
pub fn runner_session_file(id: &str) -> Result<PathBuf> {
    Ok(runner_sessions_dir()?.join(format!("{}.json", id)))
}

/// Managed service tunnel runtime state directory (~/.local/share/homeboy/service-tunnels/{id}/).
pub fn service_tunnel_runtime_dir(id: &str) -> Result<PathBuf> {
    Ok(homeboy_data()?
        .join("service-tunnels")
        .join(sanitize_path_segment(id)))
}

/// Managed service tunnel runtime state file.
pub fn service_tunnel_runtime_state_file(id: &str) -> Result<PathBuf> {
    Ok(service_tunnel_runtime_dir(id)?.join("state.json"))
}

/// Preview ingress route declarations (~/.config/homeboy/preview-ingress/routes/).
pub fn preview_ingress_routes_dir() -> Result<PathBuf> {
    Ok(homeboy()?.join("preview-ingress").join("routes"))
}

/// Preview ingress route declaration file.
pub fn preview_ingress_route_file(id: &str) -> Result<PathBuf> {
    Ok(preview_ingress_routes_dir()?.join(format!("{}.json", sanitize_path_segment(id))))
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
    use crate::test_support::with_isolated_home;

    #[test]
    fn join_remote_path_allows_absolute_paths_without_base() {
        assert_eq!(
            join_remote_path(None, "/var/log/syslog").unwrap(),
            "/var/log/syslog"
        );
    }

    #[test]
    fn artifact_root_defaults_under_homeboy_data() {
        with_isolated_home(|home| {
            assert_eq!(
                artifact_root().expect("artifact root"),
                home.path().join(".local/share/homeboy/artifacts")
            );
        });
    }

    #[test]
    fn artifact_root_uses_configured_value() {
        with_isolated_home(|home| {
            let configured = home.path().join("custom-artifacts");
            crate::core::defaults::save_config(&crate::core::defaults::HomeboyConfig {
                artifact_root: Some(configured.to_string_lossy().to_string()),
                ..crate::core::defaults::HomeboyConfig::default()
            })
            .expect("save config");

            assert_eq!(artifact_root().expect("artifact root"), configured);
        });
    }

    #[test]
    fn artifact_root_prefers_env_over_config() {
        with_isolated_home(|home| {
            let configured = home.path().join("config-artifacts");
            let env_root = home.path().join("env-artifacts");
            crate::core::defaults::save_config(&crate::core::defaults::HomeboyConfig {
                artifact_root: Some(configured.to_string_lossy().to_string()),
                ..crate::core::defaults::HomeboyConfig::default()
            })
            .expect("save config");
            std::env::set_var("HOMEBOY_ARTIFACT_ROOT", &env_root);

            assert_eq!(artifact_root().expect("artifact root"), env_root);
        });
    }

    #[test]
    fn test_set_artifact_root_override() {
        with_isolated_home(|home| {
            let env_root = home.path().join("env-artifacts");
            let override_root = home.path().join("override-artifacts");
            std::env::set_var("HOMEBOY_ARTIFACT_ROOT", &env_root);
            set_artifact_root_override(Some(override_root.clone()));

            assert_eq!(artifact_root().expect("artifact root"), override_root);
        });
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
    fn test_runner_sessions_dir_under_homeboy_dir() {
        let path = runner_sessions_dir().expect("runner_sessions_dir resolves");
        assert!(path.ends_with("runner-sessions"), "got {}", path.display());
        assert!(path.parent().expect("parent").ends_with("homeboy"));
    }

    #[test]
    fn test_runner_session_file_uses_id_filename() {
        let path = runner_session_file("lab-box").expect("runner_session_file resolves");
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("lab-box.json")
        );
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("runner-sessions")
        );
    }
}
