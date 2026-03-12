use crate::component::{
    associated_projects, mutate_portable, rename_component, resolve_effective, Component,
};
use crate::config;
use crate::error::{Error, Result};
use crate::output::{MergeOutput, MergeResult};
use serde_json::Value;
use std::path::Path;

/// Set the changelog target for a component's configuration.
pub fn set_changelog_target(component_id: &str, file_path: &str) -> Result<()> {
    mutate_portable(component_id, |component| {
        component.changelog_target = Some(file_path.to_string());
        Ok(())
    })?;
    Ok(())
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let id = id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "component_id",
            "Component ID is required for component mutation",
            None,
            None,
        )
    })?;

    let raw = config::read_json_spec_to_string(json_spec)?;
    if config::is_json_array(&raw) {
        return Err(Error::validation_invalid_argument(
            "component",
            "Bulk component mutation is no longer supported. Mutate repo-owned homeboy.json one component at a time.",
            None,
            None,
        ));
    }

    let patch: Value = config::from_str(&raw)?;

    if let Some(json_id) = patch.get("id").and_then(|v| v.as_str()) {
        if json_id != id {
            rename(id, json_id)?;
            return merge(Some(json_id), json_spec, replace_fields);
        }
    }

    let component = mutate_portable(id, |component| {
        let fields = config::merge_config(component, patch.clone(), replace_fields)?;
        if fields.updated_fields.is_empty() {
            return Err(Error::validation_invalid_argument(
                "merge",
                "Merge patch cannot be empty",
                None,
                None,
            ));
        }
        Ok(())
    })?;

    let updated_fields = match patch {
        Value::Object(obj) => obj.keys().cloned().collect(),
        _ => vec![],
    };

    let _ = component;
    Ok(MergeOutput::Single(MergeResult {
        id: id.to_string(),
        updated_fields,
    }))
}

pub fn delete_safe(id: &str) -> Result<()> {
    let component = resolve_effective(Some(id), None, None)?;
    let local_path = Path::new(&component.local_path);
    let config_path = local_path.join("homeboy.json");

    if !config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("No homeboy.json found for component '{}'", id),
            Some(id.to_string()),
            None,
        ));
    }

    if !associated_projects(id)?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Cannot delete component '{}' while projects still reference it",
                id
            ),
            Some(id.to_string()),
            None,
        ));
    }

    std::fs::remove_file(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("remove {}", config_path.display())),
        )
    })
}

pub fn rename(id: &str, new_id: &str) -> Result<Component> {
    rename_component(id, new_id)
}
