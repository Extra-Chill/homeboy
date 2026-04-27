use crate::component::{discover_from_portable, inventory, load, Component};
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

pub fn resolve_artifact(component: &Component) -> Option<String> {
    if let Some(ref artifact) = component.build_artifact {
        return Some(artifact.clone());
    }

    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = crate::extension::load_extension(extension_id) {
                if let Some(ref build) = manifest.build {
                    if let Some(ref pattern) = build.artifact_pattern {
                        let resolved = pattern
                            .replace("{component_id}", &component.id)
                            .replace("{local_path}", &component.local_path);
                        return Some(resolved);
                    }
                }
            }
        }
    }

    None
}

/// Validates component local_path is usable (absolute and exists).
pub fn validate_local_path(component: &Component) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(&component.local_path);
    let path = PathBuf::from(expanded.as_ref());

    if !path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' has relative local_path '{}' which cannot be resolved. Use absolute path like /Users/chubes/path/to/component",
                component.id, component.local_path
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!(
            "Set absolute path: homeboy component set {} --local-path \"/full/path/to/{}\"",
            component.id, component.local_path
        ))
        .with_hint("Use 'pwd' in the component directory to get the absolute path".to_string()));
    }

    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' local_path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!("Verify the path exists: ls -la {}", path.display()))
        .with_hint(format!(
            "Update path: homeboy component set {} --local-path \"/correct/path\"",
            component.id
        )));
    }

    Ok(path)
}

/// Detect component ID from current working directory.
fn detect_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let components = inventory().ok()?;

    for component in components {
        let expanded = shellexpand::tilde(&component.local_path);
        let local_path = Path::new(expanded.as_ref());

        if cwd.starts_with(local_path) {
            return Some(component.id);
        }
    }
    None
}

/// Check if the CWD (or its git root) is a checkout of the given component.
///
/// Returns the CWD-discovered component when the portable `homeboy.json` in the
/// current directory (or git root) has a matching `id`. This means the user is
/// standing inside a clone of this component and intends to operate on it,
/// even if the registered `local_path` points elsewhere (#694).
fn prefer_cwd_for_component(component_id: &str) -> Option<Component> {
    let cwd = std::env::current_dir().ok()?;

    // Check CWD directly
    if let Some(discovered) = discover_from_portable(&cwd) {
        if discovered.id == component_id {
            return Some(discovered);
        }
    }

    // Check git root if different from CWD
    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(discovered) = discover_from_portable(&git_root) {
                if discovered.id == component_id {
                    return Some(discovered);
                }
            }
        }
    }

    None
}

/// Find the git root directory for a given path.
pub(crate) fn detect_git_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Resolve a Component from an optional ID, with CWD auto-discovery fallback.
pub fn resolve(id: Option<&str>) -> Result<Component> {
    if let Some(id) = id {
        return load(id);
    }

    if let Some(detected_id) = detect_from_cwd() {
        return load(&detected_id);
    }

    let cwd = std::env::current_dir().map_err(|e| Error::internal_io(e.to_string(), None))?;

    if let Some(component) = discover_from_portable(&cwd) {
        return Ok(component);
    }

    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(component) = discover_from_portable(&git_root) {
                return Ok(component);
            }
        }
    }

    let mut hints = vec![
        "Provide a component ID: homeboy <command> <component-id>".to_string(),
        "Or run from a directory containing homeboy.json".to_string(),
    ];
    if detect_from_cwd().is_none() {
        hints.push("Initialize the repo: homeboy component create --local-path .".to_string());
        hints.push(
            "Or attach the repo to a project: homeboy project components attach-path <project> ."
                .to_string(),
        );
    }

    Err(Error::validation_invalid_argument(
        "component_id",
        "No component ID provided and no homeboy.json found in current directory",
        None,
        Some(hints),
    ))
}

/// Resolve the effective component for runtime operations.
pub fn resolve_effective(
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::project::Project>,
) -> Result<Component> {
    if let (Some(project), Some(id)) = (project, id) {
        let mut component = crate::project::resolve_project_component(project, id)?;
        if let Some(path) = path_override {
            component.local_path = path.to_string();
        }
        return Ok(component);
    }

    if let Some(id) = id {
        if let Some(path) = path_override {
            if let Some(mut discovered) = discover_from_portable(Path::new(path)) {
                discovered.id = id.to_string();
                discovered.local_path = path.to_string();
                discovered.resolve_remote_path();
                Ok(discovered)
            } else {
                // Fallback: create a synthetic component when --path is
                // explicitly provided but the directory has no homeboy.json.
                // This supports ad-hoc operations on unregistered projects.
                Ok(Component {
                    id: id.to_string(),
                    local_path: path.to_string(),
                    ..Component::default()
                })
            }
        } else {
            let id_path = Path::new(id);
            if id_path.is_dir() {
                if let Some(mut discovered) = discover_from_portable(id_path) {
                    discovered.local_path = id.to_string();
                    discovered.resolve_remote_path();
                    return Ok(discovered);
                }

                let name = id_path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                return Ok(Component {
                    id: name,
                    local_path: id.to_string(),
                    ..Component::default()
                });
            }

            // No --path provided. Before falling back to the registry, check
            // if the CWD (or its git root) is a checkout of this component.
            // This ensures `homeboy test foo` from a different clone of `foo`
            // operates on the current checkout, not the registered local_path (#694).
            if let Some(cwd_component) = prefer_cwd_for_component(id) {
                return Ok(cwd_component);
            }
            load(id)
        }
    } else {
        let mut component = resolve(None)?;
        if let Some(path) = path_override {
            component.local_path = path.to_string();
        }
        Ok(component)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ScopedExtensionConfig;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn cwd_lock() -> &'static Mutex<()> {
        CWD_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = cwd_lock().lock().expect("cwd lock");
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir).expect("set cwd");
        let result = f();
        std::env::set_current_dir(previous).expect("restore cwd");
        result
    }

    #[test]
    fn test_resolve_artifact() {
        let explicit = Component {
            id: "explicit".to_string(),
            local_path: "/tmp/explicit".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };

        assert_eq!(
            resolve_artifact(&explicit),
            Some("dist/plugin.zip".to_string())
        );

        let mut extensions = HashMap::new();
        extensions.insert(
            "unknown-extension".to_string(),
            ScopedExtensionConfig::default(),
        );
        let missing_extension = Component {
            id: "missing-extension".to_string(),
            local_path: "/tmp/missing-extension".to_string(),
            extensions: Some(extensions),
            ..Component::default()
        };

        assert_eq!(resolve_artifact(&missing_extension), None);
    }

    #[test]
    fn test_validate_local_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let component = Component {
            id: "valid".to_string(),
            local_path: dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        assert_eq!(
            validate_local_path(&component).expect("valid path"),
            dir.path()
        );

        let relative = Component {
            id: "relative".to_string(),
            local_path: "relative/path".to_string(),
            ..Component::default()
        };
        assert!(validate_local_path(&relative).is_err());
    }

    #[test]
    fn test_detect_from_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");

        with_cwd(dir.path(), || {
            assert_eq!(detect_from_cwd(), None);
        });
    }

    #[test]
    fn test_detect_git_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        assert_eq!(detect_git_root(&repo), Some(repo.canonicalize().unwrap()));
    }

    #[test]
    fn resolve_effective_accepts_raw_directory_as_positional_component() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("raw-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(Some(repo.to_str().unwrap()), None, None)
            .expect("raw directory should resolve");

        assert_eq!(component.id, "raw-repo");
        assert_eq!(component.local_path, repo.to_string_lossy());
    }

    #[test]
    fn resolve_effective_preserves_explicit_path_override_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("override-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(Some("registered-id"), repo.to_str(), None)
            .expect("explicit path override should resolve");

        assert_eq!(component.id, "registered-id");
        assert_eq!(component.local_path, repo.to_string_lossy());
    }
}
