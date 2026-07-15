use homeboy_error::{Error, Result};
use std::env;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

mod locations;
mod rigs;
mod runtime;

pub use locations::*;
pub use rigs::*;
pub use runtime::*;

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

/// Resolver hook for the config-level `artifact_root` value.
///
/// `paths` is a foundation layer and must not depend on the config/`defaults`
/// layer (that would create a paths <-> defaults dependency cycle, blocking the
/// crate split). Instead, the config layer registers a resolver here at startup
/// via [`set_config_artifact_root_resolver`]; `artifact_root()` calls it to
/// honor the global config override without an inbound dependency on `defaults`.
type ConfigArtifactRootResolver = fn() -> Option<String>;

fn config_artifact_root_resolver() -> &'static Mutex<Option<ConfigArtifactRootResolver>> {
    static RESOLVER: OnceLock<Mutex<Option<ConfigArtifactRootResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| Mutex::new(None))
}

/// Register the resolver that supplies the config-level `artifact_root` override.
///
/// Called once during startup by the config layer. Keeps `paths` free of any
/// compile-time dependency on `defaults`.
pub fn set_config_artifact_root_resolver(resolver: ConfigArtifactRootResolver) {
    let mut guard = config_artifact_root_resolver()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(resolver);
}

fn resolved_config_artifact_root() -> Option<String> {
    let resolver = {
        let guard = config_artifact_root_resolver()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    };
    resolver.and_then(|f| f())
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
        Ok(PathBuf::from(appdata).join(homeboy_product_identity::PRODUCT_IDENTITY.config_dirname))
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
            .join(homeboy_product_identity::PRODUCT_IDENTITY.config_dirname))
    }
}

/// Global product config file path
pub fn homeboy_json() -> Result<PathBuf> {
    Ok(homeboy_product_identity::PRODUCT_IDENTITY.config_file(homeboy()?))
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
        Ok(PathBuf::from(base).join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname))
    }

    #[cfg(not(windows))]
    {
        if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
            if !xdg_data_home.trim().is_empty() {
                return Ok(PathBuf::from(xdg_data_home)
                    .join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname));
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
            .join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname))
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
        return Ok(expand_tilde_path(path));
    }

    if let Ok(path) = env::var("HOMEBOY_ARTIFACT_ROOT") {
        if !path.trim().is_empty() {
            return Ok(expand_tilde_path(path));
        }
    }

    if let Some(path) = resolved_config_artifact_root() {
        if !path.trim().is_empty() {
            return Ok(expand_tilde_path(path));
        }
    }

    Ok(homeboy_data()?.join("artifacts"))
}

/// Expand a leading tilde in a local path.
pub fn expand_tilde_path(path: impl AsRef<Path>) -> PathBuf {
    let raw = path.as_ref().to_string_lossy();
    PathBuf::from(shellexpand::tilde(&raw).into_owned())
}

pub fn sanitize_path_segment(segment: &str) -> String {
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

/// Normalize a local path lexically without touching the filesystem.
///
/// This removes `.` segments, collapses internal `..` segments, and preserves
/// leading `..` segments for relative paths. Absolute paths do not escape above
/// their root. Use this before containment checks when the target path may not
/// exist yet.
pub fn normalize_local_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let mut prefix: Option<OsString> = None;
    let mut rooted = false;
    let mut segments: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
            Component::RootDir => {
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if segments.last().is_some_and(|segment| segment != "..") {
                    segments.pop();
                } else if !rooted {
                    segments.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => segments.push(value.to_os_string()),
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if rooted {
        normalized.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for segment in segments {
        normalized.push(segment);
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

/// Render each path component as a lossy UTF-8 string, in order.
///
/// Centralizes the `path.components()` + `as_os_str().to_string_lossy()`
/// traversal so component-walking helpers (temp-marker detection, case
/// insensitive comparison, etc.) share one primitive instead of each
/// reimplementing the same iteration.
pub fn path_component_strings(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Return whether `path` is inside `root` after lexical normalization.
pub fn local_path_is_contained(root: impl AsRef<Path>, path: impl AsRef<Path>) -> bool {
    let root = normalize_local_path(root);
    let path = normalize_local_path(path);

    path == root || path.starts_with(root)
}

/// Resolve a local path against a root and reject paths that escape that root.
///
/// Relative candidates are resolved below `root`. Absolute candidates are
/// accepted only when they already point inside `root` after lexical
/// normalization.
pub fn resolve_contained_local_path(
    root: impl AsRef<Path>,
    candidate: impl AsRef<Path>,
    field: &str,
) -> Result<PathBuf> {
    let root = normalize_local_path(root);
    let candidate = candidate.as_ref();
    let resolved = if candidate.is_absolute() {
        normalize_local_path(candidate)
    } else {
        normalize_local_path(root.join(candidate))
    };

    if local_path_is_contained(&root, &resolved) {
        Ok(resolved)
    } else {
        Err(Error::validation_invalid_argument(
            field,
            format!(
                "Path '{}' escapes root '{}'",
                candidate.display(),
                root.display()
            ),
            Some(candidate.to_string_lossy().to_string()),
            Some(vec![
                "Use a relative path inside the configured root".to_string(),
                "Use an absolute path that starts inside the configured root".to_string(),
            ]),
        ))
    }
}

/// Extension directory path
pub fn extension(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id))
}

/// Extension manifest file path
pub fn extension_manifest(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id).join(format!("{}.json", id)))
}

/// Agent runtime manifest file path
pub fn agent_runtime_manifest(id: &str) -> Result<PathBuf> {
    Ok(agent_runtimes()?.join(id).join(format!("{}.json", id)))
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

pub fn resolve_optional_base_path(base_path: Option<&str>) -> Option<&str> {
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

pub fn join_remote_child(base_path: Option<&str>, dir: &str, child: &str) -> Result<String> {
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

/// The lexical authorization failure for a runner-side path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePathAuthorizationError {
    NotAbsolute,
    ContainsParentDir,
    OutsideAllowedRoots,
}

/// The containment semantics selected by a runner artifact caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePathRootContainment {
    RemoteString,
    NativePath,
}

/// Normalize a remote root without accessing the remote filesystem.
pub fn normalize_remote_root(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Return whether an absolute remote path is contained by a remote root.
pub fn remote_path_is_within_root(path: &str, root: &str) -> bool {
    let root = normalize_remote_root(root);
    path == root || path.starts_with(&format!("{root}/"))
}

/// Validate lexical authorization for a runner-side artifact path.
///
/// This deliberately does not canonicalize filesystem paths: callers use it
/// for remote path policy, while deletion paths enforce stronger symlink-safe
/// invariants separately.
pub fn authorize_remote_artifact_path(
    path: &Path,
    allowed_roots: &[String],
    containment: RemotePathRootContainment,
) -> std::result::Result<(), RemotePathAuthorizationError> {
    if !path.is_absolute() {
        return Err(RemotePathAuthorizationError::NotAbsolute);
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RemotePathAuthorizationError::ContainsParentDir);
    }
    if allowed_roots.iter().any(|root| match containment {
        RemotePathRootContainment::RemoteString => {
            remote_path_is_within_root(&path.display().to_string(), root)
        }
        RemotePathRootContainment::NativePath => path == Path::new(root) || path.starts_with(root),
    }) {
        Ok(())
    } else {
        Err(RemotePathAuthorizationError::OutsideAllowedRoots)
    }
}
