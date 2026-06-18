use crate::core::component::{discover_from_portable, portable::read_portable_config, Component};
use crate::core::engine::local_files::FileSystem;
use crate::core::error::{Error, Result};
use crate::core::extension;
use crate::core::project;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ComponentReconcileReport {
    pub component_id: String,
    pub registration_path: String,
    pub registered_local_path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovered_local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    pub applied: bool,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ComponentLocalPathDiagnostic {
    pub component_id: String,
    pub local_path: String,
    pub exists: bool,
    pub is_git_checkout: bool,
    pub is_temp_checkout: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub discovered_candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
}

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
    for project in &projects {
        for attachment in &project.components {
            if let Ok(component) = project::resolve_project_component(project, &attachment.id) {
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
fn load_standalone_components() -> Result<Vec<Component>> {
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

        // Derive component ID from filename (e.g., "data-machine.json" -> "data-machine")
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

pub(super) fn build_cleanup_paths(component: &Component) -> Vec<(String, String)> {
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

pub fn reconcile_standalone_registration(
    id: &str,
    apply: bool,
) -> Result<ComponentReconcileReport> {
    let dir = crate::core::paths::components()?;
    let registration_path = dir.join(format!("{}.json", id));
    let content = std::fs::read_to_string(&registration_path).map_err(|e| {
        Error::validation_invalid_argument(
            "component_id",
            format!("No standalone registration found for component '{id}': {e}"),
            Some(id.to_string()),
            Some(vec![
                "Run `homeboy component list` to inspect registered components".to_string(),
                "Run `homeboy component create --local-path <path>` to register a checkout"
                    .to_string(),
            ]),
        )
    })?;
    let mut json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse {}", registration_path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    let registered_local_path = json
        .get("local_path")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let diagnostic = local_path_diagnostic_for(id, &registered_local_path);
    let status = diagnostic.status.clone();
    let discovered_local_path = if status == "ok" {
        None
    } else {
        unique_candidate(diagnostic.discovered_candidates.clone())
    };
    let repair = discovered_local_path
        .as_ref()
        .map(|path| format!("Update local_path to {path}"));
    let mut applied = false;

    if apply {
        let discovered = discovered_local_path.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "apply",
                "No safe repair path was discovered",
                Some(id.to_string()),
                Some(vec![
                    "Run without --apply to inspect the current registry state".to_string(),
                ]),
            )
        })?;
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "local_path".to_string(),
                serde_json::Value::String(discovered.clone()),
            );
        }
        crate::core::component::portable::validate_component_remote_urls(&json)?;
        let updated = crate::core::config::to_string_pretty(&json)?;
        crate::core::engine::local_files::write_file_atomic(
            &registration_path,
            &updated,
            &format!(
                "write standalone registration {}",
                registration_path.display()
            ),
        )?;
        applied = true;
    }

    Ok(ComponentReconcileReport {
        component_id: id.to_string(),
        registration_path: registration_path.display().to_string(),
        registered_local_path,
        status,
        discovered_local_path,
        repair,
        applied,
    })
}

pub fn local_path_diagnostic(component: &Component) -> ComponentLocalPathDiagnostic {
    local_path_diagnostic_for(&component.id, &component.local_path)
}

fn local_path_diagnostic_for(id: &str, local_path: &str) -> ComponentLocalPathDiagnostic {
    let path = Path::new(local_path);
    let exists = path.exists();
    let git_root_path = detect_git_root(path);
    let is_git_checkout = git_root_path.is_some();
    let is_temp_checkout = is_temp_checkout_path(path);
    let status = reconcile_status(path, is_git_checkout, is_temp_checkout);
    let discovered_candidates = if status == "ok" {
        Vec::new()
    } else {
        discover_reconcile_candidates(id, path)
    };
    let unique_candidate = unique_candidate(discovered_candidates.clone());
    let warning = if status == "ok" {
        None
    } else {
        Some(match status.as_str() {
            "temp_checkout" => format!(
                "Component '{id}' local_path points at a temporary/opencode checkout: {local_path}"
            ),
            "missing" => format!("Component '{id}' local_path does not exist: {local_path}"),
            "non_git" => format!("Component '{id}' local_path is not a git checkout: {local_path}"),
            "missing_local_path" => format!("Component '{id}' has no local_path"),
            _ => format!("Component '{id}' local_path needs attention: {local_path}"),
        })
    };
    let repair_command = if let Some(candidate) = unique_candidate {
        if standalone_registration_exists(id) {
            Some(format!(
                "homeboy component reconcile {id} --apply # updates local_path to {}",
                shell_quote_for_hint(&candidate)
            ))
        } else {
            Some(format!(
                "homeboy component set {id} --local-path {}",
                shell_quote_for_hint(&candidate)
            ))
        }
    } else if status != "ok" {
        Some(format!(
            "homeboy component set {id} --local-path <stable-checkout-path>"
        ))
    } else {
        None
    };

    ComponentLocalPathDiagnostic {
        component_id: id.to_string(),
        local_path: local_path.to_string(),
        exists,
        is_git_checkout,
        is_temp_checkout,
        status,
        git_root: git_root_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        branch: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"])),
        head: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["rev-parse", "--short", "HEAD"])),
        remote_url: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["config", "--get", "remote.origin.url"])),
        upstream: git_root_path.as_ref().and_then(|path| {
            git_output(
                path,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
        }),
        discovered_candidates,
        warning,
        repair_command,
    }
}

fn reconcile_status(path: &Path, is_git_checkout: bool, is_temp_checkout: bool) -> String {
    if path.as_os_str().is_empty() {
        return "missing_local_path".to_string();
    }
    if !path.exists() {
        return "missing".to_string();
    }
    if !is_git_checkout {
        return "non_git".to_string();
    }
    if is_temp_checkout {
        return "temp_checkout".to_string();
    }
    "ok".to_string()
}

fn discover_reconcile_candidates(id: &str, registered_path: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    if let Some(parent) = registered_path.parent() {
        roots.insert(parent.to_path_buf());
    }
    for root in stable_workspace_roots(registered_path) {
        roots.insert(root);
    }

    let mut candidates = BTreeSet::new();
    for root in roots {
        discover_candidate_children(id, &root, &mut candidates);
    }

    candidates.into_iter().collect()
}

fn discover_candidate_children(id: &str, root: &Path, candidates: &mut BTreeSet<String>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || detect_git_root(&path).is_none() || is_temp_checkout_path(&path) {
            continue;
        }
        let Some(component) = discover_from_portable(&path) else {
            continue;
        };
        if component.id == id {
            candidates.insert(path.to_string_lossy().to_string());
        }
    }
}

fn unique_candidate(candidates: Vec<String>) -> Option<String> {
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

fn stable_workspace_roots(path: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(home).join("Developer"));
    }
    if let Some(root) = stable_root_before_temp_marker(path) {
        roots.push(root);
    }
    roots.into_iter().filter(|root| root.is_dir()).collect()
}

fn stable_root_before_temp_marker(path: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    for component in path.components() {
        let segment = component.as_os_str().to_string_lossy();
        if segment == "opencode" || segment == "tmp" || segment == "Temp" || segment == "T" {
            return if root.as_os_str().is_empty() {
                None
            } else {
                Some(root)
            };
        }
        root.push(component.as_os_str());
    }
    None
}

fn is_temp_checkout_path(path: &Path) -> bool {
    let rendered = path.to_string_lossy();
    rendered.contains("/opencode/")
        || rendered.contains("/tmp/opencode/")
        || rendered.contains("/Temp/")
        || rendered.contains("/T/opencode/")
}

fn detect_git_root(path: &Path) -> Option<PathBuf> {
    if path.join(".git").exists() {
        return Some(path.to_path_buf());
    }

    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn shell_quote_for_hint(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn standalone_registration_exists(id: &str) -> bool {
    crate::core::paths::components()
        .map(|dir| dir.join(format!("{id}.json")).exists())
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    // Tests that override `HOME` to redirect `paths::components()` are
    // inherently racy when run in parallel because environment variables
    // are process-wide. Rather than `#[ignore]`-ing them (which skips
    // coverage in default `cargo test` runs), we serialize every test in
    // every module that touches `HOME` through `test_support::home_env_guard()`.
    // Acquire the guard via `with_home_override()` before any `set_var("HOME", ...)`
    // and the guard's `Drop` restores the previous value; parallel test runners
    // block on the mutex instead of racing on the env var.
    //
    /// Serialized guard for tests that override `HOME`.
    ///
    /// Acquires the shared HOME env guard, snapshots the current `HOME`, and
    /// installs the test-supplied override. When the guard is dropped the
    /// previous `HOME` is restored and the lock is released.
    ///
    /// Panics on a poisoned mutex, which can only happen if a previous
    /// test panicked while holding the guard — in that case the test
    /// runner is already reporting a failure, so a follow-up panic here
    /// is fine.
    struct HomeGuard {
        previous: Option<String>,
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    fn with_home_override(new_home: &std::path::Path) -> HomeGuard {
        let lock = crate::test_support::home_env_guard();
        let previous = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", new_home.to_string_lossy().as_ref()) };
        HomeGuard {
            previous,
            _lock: lock,
        }
    }

    /// Helper: create a standalone component JSON file in a directory.
    fn write_standalone_json(dir: &std::path::Path, id: &str, local_path: &str) {
        let path = dir.join(format!("{}.json", id));
        let json = serde_json::json!({
            "local_path": local_path,
            "remote_path": format!("wp-content/plugins/{}", id),
            "extensions": { "wordpress": {} },
            "auto_cleanup": false
        });
        fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    }

    fn init_git_repo(path: &std::path::Path) {
        fs::create_dir_all(path).expect("repo dir");
        let output = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(path)
            .output()
            .expect("git init should run");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_portable_id(path: &std::path::Path, id: &str) {
        fs::write(
            path.join("homeboy.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "id": id })).unwrap(),
        )
        .expect("homeboy.json");
    }

    #[test]
    fn write_standalone_registration_rejects_blank_id() {
        let component = Component::new(
            String::new(),
            "/tmp/test".to_string(),
            "wp-content/plugins/test".to_string(),
            None,
        );

        let result = write_standalone_registration(&component);
        assert!(result.is_err(), "Should reject blank ID");
    }

    #[test]
    fn standalone_prefers_portable_config_when_available() {
        // This test calls load_standalone_components() which reads from
        // paths::components(). We set HOME to an isolated temp dir.
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        // Also create empty projects dir so inventory doesn't fail
        let projects_dir = dir.path().join(".config").join("homeboy").join("projects");
        fs::create_dir_all(&projects_dir).unwrap();

        // Create a repo directory with homeboy.json
        let repo_dir = dir.path().join("my-plugin");
        fs::create_dir_all(&repo_dir).unwrap();

        let portable = serde_json::json!({
            "id": "my-plugin",
            "version_targets": [{"file": "plugin.php", "pattern": "Version:\\s*([0-9.]+)"}],
            "changelog_target": "CHANGELOG.md",
            "extensions": {"wordpress": {}}
        });
        fs::write(
            repo_dir.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .unwrap();

        // Create standalone registration pointing to repo
        let standalone = serde_json::json!({
            "local_path": repo_dir.to_string_lossy(),
            "remote_path": "wp-content/plugins/my-plugin"
        });
        fs::write(
            config_components.join("my-plugin.json"),
            serde_json::to_string_pretty(&standalone).unwrap(),
        )
        .unwrap();

        // Override HOME via the serialized guard so parallel tests can't
        // race on this process-global env var. See the HOME_LOCK comment.
        let _home = with_home_override(dir.path());

        let result = load_standalone_components();

        let components = result.unwrap();
        let plugin = components
            .iter()
            .find(|c| c.id == "my-plugin")
            .expect("Should find my-plugin");

        // Should have data from portable config
        assert!(
            plugin.version_targets.is_some(),
            "Should have version_targets from portable config"
        );
        assert_eq!(
            plugin.changelog_target.as_deref(),
            Some("CHANGELOG.md"),
            "Should have changelog_target from portable config"
        );

        // Should have remote_path from standalone (not in portable)
        assert_eq!(
            plugin.remote_path, "wp-content/plugins/my-plugin",
            "Should inherit remote_path from standalone registration"
        );
    }

    #[test]
    fn portable_config_fields_override_standalone_registration() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let repo_dir = dir.path().join("repo-owned-plugin");
        fs::create_dir_all(&repo_dir).unwrap();

        fs::write(
            repo_dir.join("homeboy.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "repo-owned-plugin",
                "remote_path": "portable/remote-path",
                "build_artifact": "portable.zip",
                "extensions": { "portable-extension": {} }
            }))
            .unwrap(),
        )
        .unwrap();

        fs::write(
            config_components.join("repo-owned-plugin.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "local_path": repo_dir.to_string_lossy(),
                "remote_path": "standalone/remote-path",
                "build_artifact": "standalone.zip",
                "extensions": { "standalone-extension": {} }
            }))
            .unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());

        let components = load_standalone_components().unwrap();
        let plugin = components
            .iter()
            .find(|c| c.id == "repo-owned-plugin")
            .expect("component should load");

        assert_eq!(plugin.local_path, repo_dir.to_string_lossy());
        assert_eq!(plugin.remote_path, "portable/remote-path");
        assert_eq!(plugin.build_artifact.as_deref(), Some("portable.zip"));
        assert!(plugin
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key("portable-extension")));
        assert!(!plugin
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key("standalone-extension")));
    }

    #[test]
    fn load_standalone_skips_missing_local_path() {
        let dir = TempDir::new().unwrap();

        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        // Write a component with empty local_path
        let json = serde_json::json!({
            "local_path": "",
            "remote_path": "wp-content/plugins/broken"
        });
        fs::write(
            config_components.join("broken.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());
        let result = load_standalone_components();

        let components = result.unwrap();
        assert!(
            components.is_empty(),
            "Should skip components with empty local_path"
        );
    }

    #[test]
    fn load_standalone_skips_non_json_files() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        // Create a non-JSON file
        fs::write(config_components.join("readme.txt"), "not a component").unwrap();
        // Create an invalid JSON file
        fs::write(config_components.join("broken.json"), "not valid json").unwrap();

        let _home = with_home_override(dir.path());
        let result = load_standalone_components();

        let components = result.unwrap();
        assert!(
            components.is_empty(),
            "Should skip non-JSON and invalid JSON files"
        );
    }

    #[test]
    fn load_standalone_reads_json_files() {
        let dir = TempDir::new().unwrap();

        // Create the ~/.config/homeboy/components/ directory structure
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        // Create a fake component directory
        let repo_dir = dir.path().join("my-plugin");
        fs::create_dir_all(&repo_dir).unwrap();

        write_standalone_json(&config_components, "my-plugin", &repo_dir.to_string_lossy());

        let _home = with_home_override(dir.path());
        let result = load_standalone_components();

        let components = result.unwrap();
        assert!(
            components.iter().any(|c| c.id == "my-plugin"),
            "Should find my-plugin from standalone files. Found: {:?}",
            components.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stale_standalone_path_discovers_renamed_sibling_portable_component() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let workspace = dir.path().join("workspace");
        let stale_path = workspace.join("old-plugin");
        let renamed_path = workspace.join("new-plugin");
        fs::create_dir_all(&renamed_path).unwrap();

        let standalone = serde_json::json!({
            "local_path": stale_path.to_string_lossy(),
            "remote_path": "wp-content/plugins/old-plugin"
        });
        fs::write(
            config_components.join("old-plugin.json"),
            serde_json::to_string_pretty(&standalone).unwrap(),
        )
        .unwrap();

        let portable = serde_json::json!({
            "id": "new-plugin",
            "local_path": renamed_path.to_string_lossy(),
            "remote_path": "wp-content/plugins/new-plugin",
            "changelog_target": "CHANGELOG.md"
        });
        fs::write(
            renamed_path.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());
        let components = load_standalone_components().unwrap();

        let renamed = components
            .iter()
            .find(|component| component.id == "new-plugin")
            .expect("renamed sibling component should be discovered from homeboy.json");
        assert_eq!(renamed.local_path, renamed_path.to_string_lossy());
        assert!(
            !components
                .iter()
                .any(|component| component.id == "old-plugin"),
            "stale standalone path should not re-register the old component id"
        );
    }

    #[test]
    fn reconcile_reports_safe_sibling_repair_without_applying() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let stale_path = workspace.join("old-homeboy");
        let checkout = workspace.join("homeboy");
        fs::create_dir_all(checkout.join(".git")).unwrap();
        fs::write(
            checkout.join("homeboy.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "id": "homeboy" })).unwrap(),
        )
        .unwrap();
        write_standalone_json(&config_components, "homeboy", &stale_path.to_string_lossy());

        let _home = with_home_override(dir.path());
        let report = reconcile_standalone_registration("homeboy", false).unwrap();

        assert_eq!(report.status, "missing");
        assert_eq!(
            report.discovered_local_path.as_deref(),
            Some(checkout.to_string_lossy().as_ref())
        );
        assert!(!report.applied);

        let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
        assert!(raw.contains(stale_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn reconcile_apply_updates_stale_local_path_when_candidate_is_unique() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let stale_path = workspace.join("old-homeboy");
        let checkout = workspace.join("homeboy");
        fs::create_dir_all(checkout.join(".git")).unwrap();
        fs::write(
            checkout.join("homeboy.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "id": "homeboy" })).unwrap(),
        )
        .unwrap();
        write_standalone_json(&config_components, "homeboy", &stale_path.to_string_lossy());

        let _home = with_home_override(dir.path());
        let report = reconcile_standalone_registration("homeboy", true).unwrap();

        assert!(report.applied);
        let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json.get("local_path").and_then(|value| value.as_str()),
            Some(checkout.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn local_path_diagnostic_flags_temp_checkout_and_reports_stable_candidate() {
        let dir = TempDir::new().unwrap();
        let stable_checkout = dir.path().join("Developer").join("homeboy");
        let temp_checkout = dir.path().join("opencode").join("homeboy-issue-4202-temp");
        init_git_repo(&stable_checkout);
        init_git_repo(&temp_checkout);
        write_portable_id(&stable_checkout, "homeboy");
        write_portable_id(&temp_checkout, "homeboy");

        let _home = with_home_override(dir.path());
        let component = Component::new(
            "homeboy".to_string(),
            temp_checkout.to_string_lossy().to_string(),
            String::new(),
            None,
        );

        let diagnostic = local_path_diagnostic(&component);

        assert_eq!(diagnostic.status, "temp_checkout");
        assert!(diagnostic.exists);
        assert!(diagnostic.is_git_checkout);
        assert!(diagnostic.is_temp_checkout);
        assert_eq!(
            diagnostic.git_root.as_deref(),
            Some(temp_checkout.to_string_lossy().as_ref())
        );
        assert_eq!(
            diagnostic.discovered_candidates,
            vec![stable_checkout.to_string_lossy().to_string()]
        );
        assert!(diagnostic
            .warning
            .as_deref()
            .unwrap_or_default()
            .contains("temporary/opencode checkout"));
        assert!(diagnostic
            .repair_command
            .as_deref()
            .unwrap_or_default()
            .contains("homeboy component set homeboy --local-path"));
    }

    #[test]
    fn reconcile_repairs_temp_checkout_to_unique_stable_candidate() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();
        let stable_checkout = dir.path().join("Developer").join("homeboy");
        let temp_checkout = dir.path().join("opencode").join("homeboy-temp");
        init_git_repo(&stable_checkout);
        init_git_repo(&temp_checkout);
        write_portable_id(&stable_checkout, "homeboy");
        write_portable_id(&temp_checkout, "homeboy");
        write_standalone_json(
            &config_components,
            "homeboy",
            &temp_checkout.to_string_lossy(),
        );

        let _home = with_home_override(dir.path());
        let report = reconcile_standalone_registration("homeboy", true).unwrap();

        assert_eq!(report.status, "temp_checkout");
        assert_eq!(
            report.discovered_local_path.as_deref(),
            Some(stable_checkout.to_string_lossy().as_ref())
        );
        assert!(report.applied);
        let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json.get("local_path").and_then(|value| value.as_str()),
            Some(stable_checkout.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn write_standalone_creates_and_reads_back() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join(".config").join("homeboy");
        fs::create_dir_all(&config_dir).unwrap();

        let _home = with_home_override(dir.path());

        let repo_dir = dir.path().join("test-plugin");
        fs::create_dir_all(&repo_dir).unwrap();

        let component = Component::new(
            "test-plugin".to_string(),
            repo_dir.to_string_lossy().to_string(),
            "wp-content/plugins/test-plugin".to_string(),
            None,
        );

        let write_result = write_standalone_registration(&component);
        assert!(
            write_result.is_ok(),
            "Should write successfully: {:?}",
            write_result.err()
        );

        // Verify we can read it back
        let read_result = load_standalone_components();

        assert!(read_result.is_ok());
        let components = read_result.unwrap();
        assert!(
            components.iter().any(|c| c.id == "test-plugin"),
            "Should find test-plugin after writing. Found: {:?}",
            components.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_standalone_preserves_existing_fields() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        // Write an existing registration with extra fields
        let existing = serde_json::json!({
            "local_path": "/old/path",
            "remote_path": "wp-content/plugins/my-comp",
            "extra_field": "preserve-me"
        });
        fs::write(
            config_components.join("my-comp.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());

        let component = Component::new(
            "my-comp".to_string(),
            "/new/path".to_string(),
            "wp-content/plugins/my-comp".to_string(),
            None,
        );

        let result = write_standalone_registration(&component);

        assert!(result.is_ok());

        let content = fs::read_to_string(config_components.join("my-comp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        // local_path should be updated
        assert_eq!(
            json.get("local_path").and_then(|v| v.as_str()),
            Some("/new/path"),
            "local_path should be updated"
        );
        // extra_field should be preserved
        assert_eq!(
            json.get("extra_field").and_then(|v| v.as_str()),
            Some("preserve-me"),
            "unknown fields should be preserved"
        );
    }

    #[test]
    fn rename_standalone_moves_pointer_to_new_component_id() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let existing = serde_json::json!({
            "local_path": "/old/path",
            "remote_path": "target/release/old-id",
            "extra_field": "preserve-me"
        });
        fs::write(
            config_components.join("old-id.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());

        let component = Component::new(
            "new-id".to_string(),
            "/new/path".to_string(),
            "target/release/new-id".to_string(),
            None,
        );

        rename_standalone_registration("old-id", &component).unwrap();

        assert!(!config_components.join("old-id.json").exists());
        let content = fs::read_to_string(config_components.join("new-id.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            json.get("local_path").and_then(|v| v.as_str()),
            Some("/new/path")
        );
        assert_eq!(
            json.get("remote_path").and_then(|v| v.as_str()),
            Some("target/release/new-id")
        );
        assert_eq!(
            json.get("extra_field").and_then(|v| v.as_str()),
            Some("preserve-me")
        );
    }

    #[test]
    fn write_standalone_rejects_preserved_invalid_remote_url() {
        let dir = TempDir::new().unwrap();
        let config_components = dir
            .path()
            .join(".config")
            .join("homeboy")
            .join("components");
        fs::create_dir_all(&config_components).unwrap();

        let existing = serde_json::json!({
            "local_path": "/old/path",
            "remote_url": "/Users/user/Developer/homeboy"
        });
        fs::write(
            config_components.join("my-comp.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let _home = with_home_override(dir.path());

        let component = Component::new(
            "my-comp".to_string(),
            "/new/path".to_string(),
            String::new(),
            None,
        );

        let result = write_standalone_registration(&component);

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "validation.invalid_argument"
        );
    }
}
