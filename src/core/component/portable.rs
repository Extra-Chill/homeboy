use crate::component::{resolve_effective, Component};
use crate::error::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Read a `homeboy.json` portable config from a repo directory.
pub fn read_portable_config(repo_path: &Path) -> Result<Option<Value>> {
    let config_path = repo_path.join("homeboy.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", config_path.display())),
        )
    })?;

    let value: Value = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse homeboy.json".to_string()),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(Some(value))
}

fn portable_component_id_from_value(portable: &Value, dir: &Path) -> Option<String> {
    portable
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
        .and_then(|id| crate::engine::identifier::slugify_id(id, "component_id").ok())
        .or_else(|| {
            let dir_name = dir.file_name()?.to_string_lossy();
            crate::engine::identifier::slugify_id(&dir_name, "component_id").ok()
        })
}

pub fn infer_portable_component_id(dir: &Path) -> Result<String> {
    let portable = read_portable_config(dir)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "local_path",
            format!("No homeboy.json found at {}", dir.display()),
            None,
            None,
        )
    })?;

    portable_component_id_from_value(&portable, dir).ok_or_else(|| {
        Error::validation_invalid_argument(
            "id",
            format!("Could not derive component ID from {}", dir.display()),
            None,
            None,
        )
    })
}

pub fn portable_json(component: &Component) -> Result<Value> {
    let mut value = serde_json::to_value(component).map_err(|error| {
        Error::validation_invalid_argument(
            "component",
            "Failed to serialize component to portable config",
            Some(error.to_string()),
            None,
        )
    })?;

    let obj = value.as_object_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            "Portable component config must serialize to an object",
            None,
            None,
        )
    })?;

    obj.insert("id".to_string(), Value::String(component.id.clone()));
    obj.remove("aliases");
    obj.remove("local_path");

    Ok(value)
}

pub fn write_portable_config(dir: &Path, component: &Component) -> Result<()> {
    let path = dir.join("homeboy.json");
    let portable = portable_json(component)?;
    let content = crate::config::to_string_pretty(&portable)?;
    crate::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write {}", path.display()),
    )
}

pub fn has_portable_config(path: &Path) -> bool {
    read_portable_config(path).ok().flatten().is_some()
}

pub fn mutate_portable<F>(id: &str, mutator: F) -> Result<Component>
where
    F: FnOnce(&mut Component) -> Result<()>,
{
    let mut component = resolve_effective(Some(id), None, None)?;
    let local_path = PathBuf::from(&component.local_path);

    if !has_portable_config(&local_path) {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Component '{}' does not have repo-owned homeboy.json. Initialize the repo first with `homeboy component create --local-path {}`",
                id,
                component.local_path
            ),
            Some(id.to_string()),
            None,
        ));
    }

    mutator(&mut component)?;
    write_portable_config(&local_path, &component)?;
    Ok(component)
}

/// Create a virtual (unregistered) Component from a directory's `homeboy.json`.
///
/// If the directory is a git repo and `remote_url` isn't set in the portable config,
/// auto-detects it from `git remote get-url origin`.
pub fn discover_from_portable(dir: &Path) -> Option<Component> {
    let portable = read_portable_config(dir).ok()??;

    let id = portable_component_id_from_value(&portable, dir)?;
    let local_path = dir.to_string_lossy().to_string();

    let mut json = portable;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("id".to_string(), Value::String(id));
        obj.insert("local_path".to_string(), Value::String(local_path));
        obj.entry("remote_path".to_string())
            .or_insert(Value::String(String::new()));

        // Auto-detect remote_url from git if not already set
        if !obj.contains_key("remote_url") {
            if let Some(url) = crate::deploy::release_download::detect_remote_url(dir) {
                obj.insert("remote_url".to_string(), Value::String(url));
            }
        }
    }

    serde_json::from_value::<Component>(json).ok()
}
