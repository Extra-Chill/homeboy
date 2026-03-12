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
pub fn detect_from_cwd() -> Option<String> {
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
                Ok(discovered)
            } else {
                Err(Error::validation_invalid_argument(
                    "local_path",
                    format!("No homeboy.json found at {}", path),
                    Some(id.to_string()),
                    None,
                ))
            }
        } else {
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
