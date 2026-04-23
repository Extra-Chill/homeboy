use crate::component::{discover_from_portable, Component};
use crate::engine::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::extension;
use crate::project;
use std::collections::HashSet;
use std::path::Path;

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

    // 1. Project-attached components (highest priority)
    for project in &projects {
        for attachment in &project.components {
            if let Ok(component) = project::resolve_project_component(project, &attachment.id) {
                if seen.insert(component.id.clone()) {
                    components.push(component);
                }
            }
        }
    }

    // 2. Standalone component registrations from ~/.config/homeboy/components/
    //    These are components registered via `component create` or legacy config
    //    that aren't attached to any project. They're still valid for local-only
    //    operations like release, version bump, and changelog.
    if let Ok(standalone) = load_standalone_components() {
        for component in standalone {
            if seen.insert(component.id.clone()) {
                components.push(component);
            }
        }
    }

    // 3. CWD portable discovery (lowest priority)
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

/// Load standalone component registrations from `~/.config/homeboy/components/`.
///
/// Each `<id>.json` file in the components directory is a registered component
/// with at minimum a `local_path`. The component ID is derived from the filename.
///
/// If the standalone file has a `local_path` and that directory contains a
/// `homeboy.json`, the portable config is merged on top (portable config is
/// the source of truth for version_targets, changelog_target, etc.).
fn load_standalone_components() -> Result<Vec<Component>> {
    let dir = crate::paths::components()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut components = Vec::new();

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
        // (it's the source of truth for version_targets, extensions, etc.)
        // and merge the standalone file's fields as fallback.
        if local_dir.exists() {
            if let Some(mut discovered) = discover_from_portable(local_dir) {
                // The portable config is authoritative, but the standalone file
                // may have fields the portable config doesn't (e.g., remote_path
                // for deploy, settings that were set via `component set`).
                if discovered.remote_path.is_empty() {
                    if let Some(rp) = json.get("remote_path").and_then(|v| v.as_str()) {
                        if !rp.is_empty() {
                            discovered.remote_path = rp.to_string();
                        }
                    }
                }

                // Ensure the ID matches the filename (canonical source)
                discovered.id = id;
                components.push(discovered);
                continue;
            }
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

    Ok(components)
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
    let dir = match crate::paths::components() {
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
/// from any directory, even without project attachment. The file contains only
/// machine-specific fields (`local_path`, `remote_path`) — the source of truth
/// for version_targets, extensions, etc. remains in the repo's `homeboy.json`.
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

    let content = crate::config::to_string_pretty(&json)?;
    crate::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write standalone registration {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // NOTE: Tests that need to override HOME are inherently flaky when run in
    // parallel because env vars are process-wide. To avoid this, tests that
    // call load_standalone_components() or write_standalone_registration()
    // through the real paths module should use `#[ignore]` and be run with
    // `cargo test -- --ignored --test-threads=1`. Tests that can work with
    // explicit dir paths should call the underlying logic directly.

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

        // Call load_standalone_components() directly, but first we need
        // to temporarily point HOME so paths::components() resolves correctly.
        let original_home = std::env::var("HOME").ok();
        // SAFETY: this is test-only, single-threaded assertion
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let result = load_standalone_components();

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

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

        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let result = load_standalone_components();

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

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

        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let result = load_standalone_components();

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

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

        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let result = load_standalone_components();

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

        let components = result.unwrap();
        assert!(
            components.iter().any(|c| c.id == "my-plugin"),
            "Should find my-plugin from standalone files. Found: {:?}",
            components.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_standalone_creates_and_reads_back() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join(".config").join("homeboy");
        fs::create_dir_all(&config_dir).unwrap();

        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let component = Component::new(
            "test-plugin".to_string(),
            "/tmp/test-plugin".to_string(),
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

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

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

        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.path().to_string_lossy().as_ref()) };

        let component = Component::new(
            "my-comp".to_string(),
            "/new/path".to_string(),
            "wp-content/plugins/my-comp".to_string(),
            None,
        );

        let result = write_standalone_registration(&component);

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            std::env::remove_var("HOME");
        }

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
}
