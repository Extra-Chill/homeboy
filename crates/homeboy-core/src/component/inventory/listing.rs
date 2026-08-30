use crate::component::{discover_from_portable, portable::read_portable_config, Component};
use crate::error::{Error, Result};
use crate::project;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_BASE_REGISTRATION_BYTES: u64 = 1024 * 1024;

// ============================================================================
// Config-root boundary (#7505)
// ============================================================================
//
// Every read below that derives from the Homeboy config root goes through one
// of the four helpers in this section. `config_root: None` means "this whole
// resolution is ambient"; `Some(root)` means "this whole resolution is rooted".
// It is never a per-read choice — a resolution that read projects from an
// injected root and standalone registrations from the ambient one would compose
// an inventory that exists in neither home.

/// The standalone component registration directory at the active boundary.
fn components_dir(config_root: Option<&Path>) -> Result<PathBuf> {
    match config_root {
        Some(config_root) => Ok(crate::paths::components_in_root(config_root)),
        None => crate::paths::components(),
    }
}

/// Configured projects at the active boundary.
fn projects_at(config_root: Option<&Path>) -> Result<Vec<project::Project>> {
    match config_root {
        Some(config_root) => project::list_in_root(config_root),
        None => project::list(),
    }
}

/// The standalone-config snapshot at the active boundary.
///
/// Loaded from the same root the resolution runs against, so the snapshot and
/// the resolution it feeds can never describe two different homes.
fn standalone_snapshot_at(
    config_root: Option<&Path>,
) -> project::StandaloneComponentConfigSnapshot {
    match config_root {
        Some(config_root) => project::StandaloneComponentConfigSnapshot::load_in_root(config_root),
        None => project::StandaloneComponentConfigSnapshot::load(),
    }
}

/// Project-component resolution at the active boundary.
fn resolve_attachment(
    config_root: Option<&Path>,
    project: &project::Project,
    component_id: &str,
    snapshot: Option<&project::StandaloneComponentConfigSnapshot>,
) -> Result<Component> {
    match config_root {
        Some(config_root) => project::resolve_project_component_with_standalone_snapshot_in_root(
            config_root,
            project,
            component_id,
            snapshot,
        ),
        None => project::resolve_project_component_with_standalone_snapshot(
            project,
            component_id,
            snapshot,
        ),
    }
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
    inventory_core(None)
}

/// [`inventory`] against an already-resolved config root (#7505).
///
/// CWD portable discovery is unchanged: it is a fact about the invocation
/// directory, not about a config root, and reads no Homeboy state.
pub fn inventory_in_root(config_root: &Path) -> Result<Vec<Component>> {
    inventory_core(Some(config_root))
}

fn inventory_core(config_root: Option<&Path>) -> Result<Vec<Component>> {
    let mut components = registered_core(config_root)?;
    let mut seen: HashSet<String> = components
        .iter()
        .map(|component| component.id.clone())
        .collect();

    // CWD portable discovery is intentionally a command-local convenience,
    // not a source of durable component ownership.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(component) = discover_from_portable(&cwd) {
            if seen.insert(component.id.clone()) {
                components.push(component);
            }
        } else if let Some(git_root) = crate::component::resolution::detect_git_root(&cwd) {
            if let Some(component) = discover_from_portable(&git_root) {
                if seen.insert(component.id.clone()) {
                    components.push(component);
                }
            }
        }
    }

    components.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(components)
}

/// List components with persisted ownership only.
///
/// This excludes portable `homeboy.json` discovery from the caller's current
/// directory, which is suitable for host-level operations such as cleanup.
pub fn registered() -> Result<Vec<Component>> {
    registered_core(None)
}

/// List persisted component registrations without opening their checkouts.
///
/// This is the inventory boundary for broad, interactive readers. It only reads
/// Homeboy's registry files, so a stale mount or a wedged Git helper in one
/// checkout cannot delay rows for every other registered component. Callers
/// that need repo-owned portable fields must explicitly use [`registered`].
pub fn registered_base() -> Result<Vec<Component>> {
    registered_base_at(None)
}

/// [`registered`] against an already-resolved config root (#7505).
pub fn registered_in_root(config_root: &Path) -> Result<Vec<Component>> {
    registered_core(Some(config_root))
}

fn registered_core(config_root: Option<&Path>) -> Result<Vec<Component>> {
    let projects = projects_at(config_root).unwrap_or_default();
    let mut components = Vec::new();
    let mut seen = HashSet::new();
    let mut add_component = |component: Component| {
        if seen.insert(component.id.clone()) {
            components.push(component);
        }
    };

    // 1. Project-attached components (highest priority)
    let standalone_snapshot = standalone_snapshot_at(config_root);
    for project in &projects {
        for attachment in &project.components {
            if let Ok(component) = resolve_attachment(
                config_root,
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
    if let Ok(standalone) = load_standalone_components_core(config_root) {
        for component in standalone {
            add_component(component);
        }
    }

    components.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(components)
}

pub(crate) fn registered_base_at(config_root: Option<&Path>) -> Result<Vec<Component>> {
    let projects = projects_at(config_root).unwrap_or_default();
    let mut components = Vec::new();
    let mut seen = HashSet::new();

    for project in projects {
        for attachment in project.components {
            if seen.insert(attachment.id.clone()) {
                let mut component = Component::new(
                    attachment.id,
                    attachment.local_path,
                    attachment.remote_path.unwrap_or_default(),
                    None,
                );
                component.deployment_provider = attachment.deployment_provider;
                components.push(component);
            }
        }
    }

    let dir = components_dir(config_root)?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if seen.contains(id) {
                continue;
            }
            let Some(content) = read_bounded_base_registration(&path) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            if let Some(component) = component_from_standalone_config(id, json) {
                seen.insert(component.id.clone());
                components.push(component);
            }
        }
    }

    components.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(components)
}

fn read_bounded_base_registration(path: &Path) -> Option<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BASE_REGISTRATION_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BASE_REGISTRATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_BASE_REGISTRATION_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Resolve one persisted component without reconstructing the full inventory.
///
/// Cook admission uses this for an explicit repository identity so stale,
/// unrelated registrations cannot trigger portable Git enrichment.
pub fn registered_by_id(id: &str) -> Result<Option<Component>> {
    registered_by_id_core(None, id)
}

/// [`registered_by_id`] against an already-resolved config root (#7505).
pub fn registered_by_id_in_root(config_root: &Path, id: &str) -> Result<Option<Component>> {
    registered_by_id_core(Some(config_root), id)
}

fn registered_by_id_core(config_root: Option<&Path>, id: &str) -> Result<Option<Component>> {
    crate::engine::identifier::validate_component_id(id)?;
    let projects = projects_at(config_root).unwrap_or_default();
    let standalone_snapshot = standalone_snapshot_at(config_root);
    for project in &projects {
        if project
            .components
            .iter()
            .any(|attachment| attachment.id == id)
        {
            if let Ok(component) =
                resolve_attachment(config_root, project, id, Some(&standalone_snapshot))
            {
                if component.id == id {
                    return Ok(Some(component));
                }
            }
        }
    }

    load_standalone_component_core(config_root, id)
}

/// Resolve one persisted component whose configured checkout is `path` without
/// materializing unrelated component configurations.
pub fn registered_by_local_path(path: &Path) -> Result<Option<Component>> {
    registered_by_local_path_core(None, path)
}

/// [`registered_by_local_path`] against an already-resolved config root (#7505).
pub fn registered_by_local_path_in_root(
    config_root: &Path,
    path: &Path,
) -> Result<Option<Component>> {
    registered_by_local_path_core(Some(config_root), path)
}

fn registered_by_local_path_core(
    config_root: Option<&Path>,
    path: &Path,
) -> Result<Option<Component>> {
    let Ok(path) = std::fs::canonicalize(path) else {
        return Ok(None);
    };
    let projects = projects_at(config_root).unwrap_or_default();
    let standalone_snapshot = standalone_snapshot_at(config_root);
    for project in &projects {
        for attachment in &project.components {
            let attachment_path =
                PathBuf::from(shellexpand::tilde(&attachment.local_path).into_owned());
            if std::fs::canonicalize(attachment_path).ok().as_ref() != Some(&path) {
                continue;
            }
            if let Ok(component) = resolve_attachment(
                config_root,
                project,
                &attachment.id,
                Some(&standalone_snapshot),
            ) {
                return Ok(Some(component));
            }
        }
    }

    let dir = components_dir(config_root)?;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    for entry in entries.flatten() {
        let registration = entry.path();
        if registration.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = registration.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(info) = read_standalone_file_core(config_root, id) else {
            continue;
        };
        let registered_path = PathBuf::from(shellexpand::tilde(&info.local_path).into_owned());
        if std::fs::canonicalize(registered_path).ok().as_ref() == Some(&path) {
            return load_standalone_component_core(config_root, id);
        }
    }

    Ok(None)
}

/// Load standalone component registrations from `~/.config/homeboy/components/`.
///
/// Each `<id>.json` file in the components directory is a registered component
/// with at minimum a `local_path`. The component ID is derived from the filename.
///
/// If the standalone file has a `local_path` and that directory contains a
/// `homeboy.json`, the portable config is merged on top (portable config is
/// the source of truth for version_targets, changelog_target, etc.).
/// Ambient sibling of [`load_standalone_components_in_root`], retained for the
/// inventory tests that still resolve their root from the process.
///
/// `#[cfg(test)]` rather than deleted: the eight callers in this module's test
/// file are the only ones left, so in a lib build it is dead code under
/// `-D warnings`. Migrating those tests onto the rooted sibling is the honest
/// follow-up; gating it keeps that a separate, reviewable change instead of a
/// rider on the rooting slice (#7505).
#[cfg(test)]
pub(super) fn load_standalone_components() -> Result<Vec<Component>> {
    load_standalone_components_core(None)
}

/// [`load_standalone_components`] at the active config-root boundary.
fn load_standalone_components_core(config_root: Option<&Path>) -> Result<Vec<Component>> {
    let dir = components_dir(config_root)?;
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

fn load_standalone_component_core(
    config_root: Option<&Path>,
    id: &str,
) -> Result<Option<Component>> {
    let path = components_dir(config_root)?.join(format!("{id}.json"));
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Ok(None);
    };
    Ok(component_from_standalone_registration(id, json))
}

fn component_from_standalone_config(id: &str, mut json: serde_json::Value) -> Option<Component> {
    let local_path = json.get("local_path")?.as_str()?.trim();
    if local_path.is_empty() {
        return None;
    }
    json.as_object_mut()?
        .insert("id".to_string(), serde_json::Value::String(id.to_string()));
    serde_json::from_value(json).ok()
}

fn component_from_standalone_registration(id: &str, json: serde_json::Value) -> Option<Component> {
    let local_path = json.get("local_path")?.as_str()?.trim();
    if local_path.is_empty() {
        return None;
    }
    let local_dir = Path::new(local_path);

    // Repo-owned fields take precedence when the targeted registration is live.
    if local_dir.exists() {
        if let Some(discovered) = discover_from_portable(local_dir) {
            let portable = read_portable_config(local_dir)
                .ok()
                .flatten()
                .unwrap_or_else(|| serde_json::json!({}));
            return Some(overlay_standalone_registration(
                id, discovered, portable, json,
            ));
        }
    } else if let Some(parent) = local_dir.parent() {
        return find_sibling_portable_component(parent, id);
    }

    component_from_standalone_config(id, json)
}

fn find_sibling_portable_component(parent: &Path, id: &str) -> Option<Component> {
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(component) = discover_from_portable(&path) else {
            continue;
        };
        if component.id == id {
            return Some(component);
        }
    }
    None
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
    extension_provides_artifact_pattern_core(None, component)
}

/// [`extension_provides_artifact_pattern`] against an already-resolved config
/// root (#7505).
pub fn extension_provides_artifact_pattern_in_root(
    config_root: &Path,
    component: &Component,
) -> bool {
    extension_provides_artifact_pattern_core(Some(config_root), component)
}

fn extension_provides_artifact_pattern_core(
    config_root: Option<&Path>,
    component: &Component,
) -> bool {
    component
        .extensions
        .as_ref()
        .map(|extensions| {
            extensions.keys().any(|extension_id| {
                crate::extension::catalog::load_extension_in_optional_root(
                    config_root,
                    extension_id,
                )
                .ok()
                .and_then(|m| m.build)
                .and_then(|b| b.artifact_pattern)
                .is_some()
            })
        })
        .unwrap_or(false)
}

pub(in crate::component) fn build_cleanup_paths(component: &Component) -> Vec<(String, String)> {
    let mut paths = Vec::new();

    let Some(extensions) = component.extensions.as_ref() else {
        return paths;
    };

    for extension_id in extensions.keys() {
        let Ok(manifest) = crate::extension::catalog::load_extension(extension_id) else {
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

/// [`list`] against an already-resolved config root (#7505).
pub fn list_in_root(config_root: &Path) -> Result<Vec<Component>> {
    inventory_in_root(config_root)
}

pub fn list_ids() -> Result<Vec<String>> {
    Ok(inventory()?
        .into_iter()
        .map(|component| component.id)
        .collect())
}

/// [`list_ids`] against an already-resolved config root (#7505).
pub fn list_ids_in_root(config_root: &Path) -> Result<Vec<String>> {
    Ok(inventory_in_root(config_root)?
        .into_iter()
        .map(|component| component.id)
        .collect())
}

pub fn load(id: &str) -> Result<Component> {
    load_core(None, id)
}

/// [`load`] against an already-resolved config root (#7505).
///
/// The registry probe, the "not attached" diagnosis and the near-miss id
/// suggestions all resolve from `config_root`, so a miss and the suggestions
/// explaining it can never describe two different homes.
pub fn load_in_root(config_root: &Path, id: &str) -> Result<Component> {
    load_core(Some(config_root), id)
}

fn load_core(config_root: Option<&Path>, id: &str) -> Result<Component> {
    if let Some(component) = registered_by_id_core(config_root, id)? {
        return Ok(component);
    }

    // Portable discovery is a command-local fallback after persisted ownership,
    // matching inventory precedence without resolving unrelated registrations.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(component) = discover_from_portable(&cwd) {
            if component.id == id {
                return Ok(component);
            }
        } else if let Some(git_root) = crate::component::resolution::detect_git_root(&cwd) {
            if git_root != cwd {
                if let Some(component) = discover_from_portable(&git_root) {
                    if component.id == id {
                        return Ok(component);
                    }
                }
            }
        }
    }

    // Component not in full inventory. Check if a standalone registration
    // file exists — this means the component was created but isn't loaded
    // into inventory (e.g., local_path doesn't exist or portable config
    // is missing). Return a specific "not attached" error with guidance.
    if let Some(standalone) = read_standalone_file_core(config_root, id) {
        let project_suggestion = suggest_project_for_attachment_core(config_root);
        return Err(Error::component_not_attached(
            id.to_string(),
            standalone.local_path,
            project_suggestion,
        ));
    }

    let suggestions = registered_id_candidates_core(config_root);
    Err(Error::component_not_found(id.to_string(), suggestions))
}

fn registered_id_candidates_core(config_root: Option<&Path>) -> Vec<String> {
    // Suggestions describe configured candidates without resolving their
    // repositories. Resolution failures remain the selected command's concern.
    let mut ids = HashSet::new();
    for project in projects_at(config_root).unwrap_or_default() {
        ids.extend(
            project
                .components
                .into_iter()
                .map(|attachment| attachment.id),
        );
    }
    if let Ok(entries) = components_dir(config_root).and_then(|dir| {
        std::fs::read_dir(&dir).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", dir.display())))
        })
    }) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                if let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    let mut ids = ids.into_iter().collect::<Vec<_>>();
    ids.sort();
    ids
}

pub fn exists(id: &str) -> bool {
    load(id).is_ok()
}

/// [`exists`] against an already-resolved config root (#7505).
pub fn exists_in_root(config_root: &Path, id: &str) -> bool {
    load_in_root(config_root, id).is_ok()
}

/// Read a standalone registration file for a component ID without loading
/// it into the full inventory. Returns a minimal struct with `local_path`
/// for error messaging when the component exists on disk but isn't loadable.
fn read_standalone_file_core(config_root: Option<&Path>, id: &str) -> Option<StandaloneFileInfo> {
    let dir = match components_dir(config_root) {
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
fn suggest_project_for_attachment_core(config_root: Option<&Path>) -> Option<String> {
    let projects = projects_at(config_root).unwrap_or_default();
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

    let dir = crate::paths::components()?;
    crate::engine::local_files::local().ensure_dir(&dir)?;

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

    crate::component::portable::validate_component_remote_urls(&json)?;

    let content = crate::config::to_string_pretty(&json)?;
    crate::engine::local_files::write_file_atomic(
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

    let dir = crate::paths::components()?;
    crate::engine::local_files::local().ensure_dir(&dir)?;
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

    crate::component::portable::validate_component_remote_urls(&json)?;
    let content = crate::config::to_string_pretty(&json)?;
    crate::engine::local_files::write_file_atomic(
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

    let dir = crate::paths::components()?;
    crate::engine::local_files::local().ensure_dir(&dir)?;

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
