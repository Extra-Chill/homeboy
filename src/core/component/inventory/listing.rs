use crate::core::component::{discover_from_portable, portable::read_portable_config, Component};
use crate::core::engine::local_files::FileSystem;
use crate::core::error::{Error, Result};
use crate::core::extension;
use crate::core::project;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Derive a runtime component inventory from project attachments, standalone
/// registrations, and portable components.
///
/// Discovery order:
/// 1. Project-attached components (authoritative for deploy config)
/// 2. Standalone component files from `~/.config/homeboy/components/` (#1131)
/// 3. CWD portable discovery (homeboy.json in working directory)
///
/// Earlier sources win on ID collision: a project-attached component takes
/// precedence over a standalone file with the same ID, which in turn takes
/// precedence over CWD discovery.
pub fn inventory() -> Result<Vec<Component>> {
    let projects = project::list().unwrap_or_default();
    let mut components = Vec::new();
    let mut seen = HashSet::new();
    let mut add_component = |component: Component| {
        if seen.insert(component.id.clone()) {
            components.push(component);
        }
    };

    // 1. Project-attached components (highest priority)
    let standalone_snapshot = project::StandaloneComponentConfigSnapshot::load();
    for project in &projects {
        for attachment in &project.components {
            if let Ok(component) = project::resolve_project_component_with_standalone_snapshot(
                project,
                &attachment.id,
                Some(&standalone_snapshot),
            ) {
                add_component(component);
            }
        }
    }

    // 2. Standalone component registrations from ~/.config/homeboy/components/
    //    These are components registered via `component create` or legacy config
    //    that aren't attached to any project. They're still valid for local-only
    //    operations like release, version bump, and changelog.
    if let Ok(standalone) = load_standalone_components() {
        for component in standalone {
            add_component(component);
        }
    }

    // 3. CWD portable discovery (lowest priority)
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(component) = discover_from_portable(&cwd) {
            add_component(component);
        } else if let Some(git_root) = crate::core::component::resolution::detect_git_root(&cwd) {
            if let Some(component) = discover_from_portable(&git_root) {
                add_component(component);
            }
        }
    }

    components.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(components)
}

/// Load standalone component registrations from `~/.config/homeboy/components/`.
///
/// Each `<id>.json` file in the components directory is a registered component
/// with at minimum a `local_path`. The component ID is derived from the filename.
///
/// If the standalone file has a `local_path` and that directory contains a
/// `homeboy.json`, the portable config is merged on top (portable config is
/// the source of truth for version_targets, changelog_target, etc.).
pub(super) fn load_standalone_components() -> Result<Vec<Component>> {
    let dir = crate::core::paths::components()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut components = Vec::new();
    let mut stale_parent_dirs = HashSet::new();

    let entries = std::fs::read_dir(&dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", dir.display()))))?;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        // Only process .json files
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        // Derive component ID from filename (e.g., "sample-plugin.json" -> "sample-plugin")
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => stem.to_string(),
            None => continue,
        };

        // Read the standalone config file
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let local_path = match json.get("local_path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => continue,
        };

        let local_dir = Path::new(&local_path);

        // If the local_path directory has a homeboy.json, prefer portable discovery
        // (it's the source of truth for repo-owned fields) and use standalone
        // data only for machine-local fields or legacy fallback values.
        if local_dir.exists() {
            if let Some(discovered) = discover_from_portable(local_dir) {
                let portable = read_portable_config(local_dir)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| serde_json::json!({}));
                let component = overlay_standalone_registration(&id, discovered, portable, json);
                components.push(component);
                continue;
            }
        } else if let Some(parent) = local_dir.parent() {
            stale_parent_dirs.insert(parent.to_path_buf());
            continue;
        }

        // No portable config available — build component from the standalone JSON.
        // Insert the id so deserialization picks it up.
        let mut json = json;
        if let Some(obj) = json.as_object_mut() {
            obj.insert("id".to_string(), serde_json::Value::String(id));
        }

        if let Ok(component) = serde_json::from_value::<Component>(json) {
            components.push(component);
        }
    }

    let mut seen_ids: HashSet<String> = components.iter().map(|c| c.id.clone()).collect();
    for parent in stale_parent_dirs {
        discover_sibling_portable_components(&parent, &mut seen_ids, &mut components);
    }

    Ok(components)
}

fn overlay_standalone_registration(
    id: &str,
    discovered: Component,
    portable: serde_json::Value,
    standalone: serde_json::Value,
) -> Component {
    let mut merged = serde_json::to_value(&discovered).unwrap_or_else(|_| serde_json::json!({}));

    if let (Some(base), Some(overrides)) = (merged.as_object_mut(), standalone.as_object()) {
        let portable = portable.as_object();
        for (key, value) in overrides {
            if key == "id" || value.is_null() || key == "local_path" {
                continue;
            }
            if portable.is_some_and(|portable| portable.contains_key(key)) {
                continue;
            }
            base.insert(key.clone(), value.clone());
        }

        if let Some(local_path) = overrides.get("local_path").filter(|value| !value.is_null()) {
            base.insert("local_path".to_string(), local_path.clone());
        }
    }

    if let Some(obj) = merged.as_object_mut() {
        obj.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    }

    serde_json::from_value::<Component>(merged).unwrap_or_else(|_| {
        let mut fallback = discovered;
        fallback.id = id.to_string();
        fallback
    })
}

/// Discover sibling repos when a standalone registration points at a path that
/// no longer exists. This catches common workspace renames (`mv old-id new-id`)
/// where the new directory already has an updated repo-owned `homeboy.json`.
fn discover_sibling_portable_components(
    parent: &Path,
    seen_ids: &mut HashSet<String>,
    components: &mut Vec<Component>,
) {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut discovered = Vec::new();
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(component) = discover_from_portable(&path) else {
            continue;
        };

        if seen_ids.insert(component.id.clone()) {
            discovered.push(component);
        }
    }

    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    components.extend(discovered);
}

/// Check if any linked extension provides an artifact pattern.
pub fn extension_provides_artifact_pattern(component: &Component) -> bool {
    component
        .extensions
        .as_ref()
        .map(|extensions| {
            extensions.keys().any(|extension_id| {
                extension::load_extension(extension_id)
                    .ok()
                    .and_then(|m| m.build)
                    .and_then(|b| b.artifact_pattern)
                    .is_some()
            })
        })
        .unwrap_or(false)
}

pub(in crate::core::component) fn build_cleanup_paths(
    component: &Component,
) -> Vec<(String, String)> {
    let mut paths = Vec::new();

    let Some(extensions) = component.extensions.as_ref() else {
        return paths;
    };

    for extension_id in extensions.keys() {
        let Ok(manifest) = extension::load_extension(extension_id) else {
            continue;
        };
        let Some(build) = manifest.build.as_ref() else {
            continue;
        };
        paths.extend(
            build
                .cleanup_paths
                .iter()
                .cloned()
                .map(|path| (extension_id.clone(), path)),
        );
    }

    paths
}

pub fn list() -> Result<Vec<Component>> {
    inventory()
}

pub fn list_ids() -> Result<Vec<String>> {
    Ok(inventory()?
        .into_iter()
        .map(|component| component.id)
        .collect())
}

pub fn load(id: &str) -> Result<Component> {
    if let Some(component) = inventory()?
        .into_iter()
        .find(|component| component.id == id)
    {
        return Ok(component);
    }

    // Component not in full inventory. Check if a standalone registration
    // file exists — this means the component was created but isn't loaded
    // into inventory (e.g., local_path doesn't exist or portable config
    // is missing). Return a specific "not attached" error with guidance.
    if let Some(standalone) = read_standalone_file(id) {
        let project_suggestion = suggest_project_for_attachment();
        return Err(Error::component_not_attached(
            id.to_string(),
            standalone.local_path,
            project_suggestion,
        ));
    }

    let suggestions = list_ids().unwrap_or_default();
    Err(Error::component_not_found(id.to_string(), suggestions))
}

pub fn exists(id: &str) -> bool {
    load(id).is_ok()
}

/// Read a standalone registration file for a component ID without loading
/// it into the full inventory. Returns a minimal struct with `local_path`
/// for error messaging when the component exists on disk but isn't loadable.
fn read_standalone_file(id: &str) -> Option<StandaloneFileInfo> {
    let dir = match crate::core::paths::components() {
        Ok(d) if d.exists() => d,
        _ => return None,
    };

    let path = dir.join(format!("{}.json", id));
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let local_path = json.get("local_path").and_then(|v| v.as_str())?;

    Some(StandaloneFileInfo {
        local_path: local_path.to_string(),
    })
}

/// Minimal info extracted from a standalone registration file for error messages.
struct StandaloneFileInfo {
    local_path: String,
}

/// If exactly one project exists, return its ID for the attach hint.
fn suggest_project_for_attachment() -> Option<String> {
    let projects = project::list().unwrap_or_default();
    if projects.len() == 1 {
        Some(projects[0].id.clone())
    } else {
        None
    }
}

/// Write a standalone component registration to `~/.config/homeboy/components/<id>.json`.
///
/// This creates a lightweight pointer file so the component is discoverable by ID
/// from any directory, even without project attachment. The file's explicit
/// machine-local field is `local_path`; other fields are legacy fallback data
/// and do not override fields present in the repo's `homeboy.json`.
pub fn write_standalone_registration(component: &Component) -> Result<()> {
    if component.id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "id",
            "Cannot write standalone registration with a blank component ID",
            None,
            None,
        ));
    }

    let dir = crate::core::paths::components()?;
    crate::core::engine::local_files::local().ensure_dir(&dir)?;

    let path = dir.join(format!("{}.json", component.id));

    // Build a minimal registration object with machine-specific fields.
    // Preserve existing fields if the file already exists (read-modify-write).
    let mut json = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "local_path".to_string(),
            serde_json::Value::String(component.local_path.clone()),
        );

        // Only write remote_path if non-empty
        if !component.remote_path.is_empty() {
            obj.insert(
                "remote_path".to_string(),
                serde_json::Value::String(component.remote_path.clone()),
            );
        }
    }

    crate::core::component::portable::validate_component_remote_urls(&json)?;

    let content = crate::core::config::to_string_pretty(&json)?;
    crate::core::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write standalone registration {}", path.display()),
    )
}

/// Write the effective component config to the standalone registry without
/// mutating the repo-owned portable `homeboy.json`.
pub fn write_standalone_component_config(component: &Component) -> Result<()> {
    if component.id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "id",
            "Cannot write standalone component config with a blank component ID",
            None,
            None,
        ));
    }

    let dir = crate::core::paths::components()?;
    crate::core::engine::local_files::local().ensure_dir(&dir)?;
    let path = dir.join(format!("{}.json", component.id));

    let mut json = serde_json::to_value(component).map_err(|error| {
        Error::validation_invalid_argument(
            "component",
            "Failed to serialize component registration",
            Some(error.to_string()),
            None,
        )
    })?;
    if let Some(obj) = json.as_object_mut() {
        obj.remove("id");
        obj.insert(
            "local_path".to_string(),
            serde_json::Value::String(component.local_path.clone()),
        );
    }

    crate::core::component::portable::validate_component_remote_urls(&json)?;
    let content = crate::core::config::to_string_pretty(&json)?;
    crate::core::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write standalone component config {}", path.display()),
    )
}

/// Move the standalone pointer file when a component ID changes, then rewrite it.
pub fn rename_standalone_registration(old_id: &str, component: &Component) -> Result<()> {
    if old_id == component.id {
        return write_standalone_registration(component);
    }

    let dir = crate::core::paths::components()?;
    crate::core::engine::local_files::local().ensure_dir(&dir)?;

    let old_path = dir.join(format!("{}.json", old_id));
    let new_path = dir.join(format!("{}.json", component.id));

    if old_path.exists() && !new_path.exists() {
        std::fs::rename(&old_path, &new_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "rename standalone registration {} to {}",
                    old_path.display(),
                    new_path.display()
                )),
            )
        })?;
    }

    write_standalone_registration(component)?;

    if old_path.exists() {
        std::fs::remove_file(&old_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "remove stale standalone registration {}",
                    old_path.display()
                )),
            )
        })?;
    }

    Ok(())
}
