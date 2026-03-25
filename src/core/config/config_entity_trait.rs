//! config_entity_trait — extracted from config.rs.

use crate::engine::identifier;
use crate::engine::local_files::{self, FileSystem};
use crate::error::Error;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::Result;
use serde_json::{Map, Value};
use heck::ToSnakeCase;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use super::find_similar_ids;
use super::to_string_pretty;
use super::from_str;
use super::create_batch;
use super::is_json_array;
use super::read_json_spec_to_string;
use super::ConfigEntity;


pub fn load<T: ConfigEntity>(id: &str) -> Result<T> {
    let path = T::config_path(id)?;
    if !path.exists() {
        // Try alias resolution before giving up
        if let Some(real_id) = resolve_alias::<T>(id) {
            let alias_path = T::config_path(&real_id)?;
            let content = local_files::local().read(&alias_path)?;
            let mut entity: T = from_str(&content)?;
            entity.set_id(real_id);
            entity.post_load(&content);
            return Ok(entity);
        }
        let suggestions = find_similar_ids::<T>(id);
        return Err(T::not_found_error(id.to_string(), suggestions));
    }
    let content = local_files::local().read(&path)?;
    let mut entity: T = from_str(&content)?;
    entity.set_id(id.to_string());
    entity.post_load(&content);
    Ok(entity)
}

/// Resolve an alias to the real entity ID by scanning all entities.
pub(crate) fn resolve_alias<T: ConfigEntity>(alias: &str) -> Option<String> {
    let alias_lower = alias.to_lowercase();
    let entities = list::<T>().ok()?;
    for entity in &entities {
        for a in entity.aliases() {
            if a.to_lowercase() == alias_lower {
                return Some(entity.id().to_string());
            }
        }
    }
    None
}

pub fn list<T: ConfigEntity>() -> Result<Vec<T>> {
    let dir = T::config_dir()?;
    let entries = local_files::local().list(&dir)?;

    let mut items: Vec<T> = entries
        .into_iter()
        .filter_map(|e| {
            // Determine the path to the JSON file and the ID
            let (json_path, id) = if e.is_dir {
                // For directories (extension structure): look for {dir}/{dir}.json
                let dir_name = e.path.file_name()?.to_string_lossy().to_string();
                let nested_json = e.path.join(format!("{}.json", dir_name));
                if nested_json.exists() {
                    (nested_json, dir_name)
                } else {
                    return None;
                }
            } else if e.is_json() {
                // For flat files: use existing behavior
                let id = e.path.file_stem()?.to_string_lossy().to_string();
                (e.path.clone(), id)
            } else {
                return None;
            };

            let content = match local_files::local().read(&json_path) {
                Ok(c) => c,
                Err(err) => {
                    log_status!(
                        "config",
                        "Warning: failed to read {}: {}",
                        json_path.display(),
                        err
                    );
                    return None;
                }
            };
            let mut entity: T = match from_str(&content) {
                Ok(e) => e,
                Err(err) => {
                    log_status!(
                        "config",
                        "Warning: failed to parse {}: {}",
                        json_path.display(),
                        err
                    );
                    return None;
                }
            };
            entity.set_id(id);
            Some(entity)
        })
        .collect();
    items.sort_by(|a, b| a.id().cmp(b.id()));
    Ok(items)
}

pub fn check_id_collision(id: &str, saving_type: &str) -> Result<()> {
    // Check each entity type using exists::<T>() which handles custom config_path
    // (e.g., extensions use {dir}/{id}/{id}.json instead of {dir}/{id}.json)
    fn check<T: ConfigEntity>(id: &str, saving_type: &str) -> Result<()> {
        if T::ENTITY_TYPE == saving_type {
            return Ok(());
        }
        if exists::<T>(id) {
            return Err(Error::config_id_collision(id, saving_type, T::ENTITY_TYPE));
        }
        Ok(())
    }

    check::<crate::project::Project>(id, saving_type)?;
    check::<crate::server::Server>(id, saving_type)?;
    check::<crate::extension::ExtensionManifest>(id, saving_type)?;
    check::<crate::fleet::Fleet>(id, saving_type)?;

    // Check if the ID collides with any existing alias
    check_alias_collision_all(id, saving_type)?;

    Ok(())
}

/// Check if a given ID or alias collides with any existing entity's aliases.
pub(crate) fn check_alias_collision_all(id: &str, saving_type: &str) -> Result<()> {
    let id_lower = id.to_lowercase();

    // Helper macro to avoid repeating for each entity type
    fn check_aliases_in<T: ConfigEntity>(id_lower: &str, saving_type: &str) -> Result<()> {
        if T::ENTITY_TYPE == saving_type {
            return Ok(());
        }
        if let Ok(entities) = list::<T>() {
            for entity in &entities {
                for alias in entity.aliases() {
                    if alias.to_lowercase() == *id_lower {
                        return Err(Error::config(format!(
                            "ID '{}' conflicts with an alias on {} '{}'",
                            id_lower,
                            T::ENTITY_TYPE,
                            entity.id()
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    check_aliases_in::<crate::project::Project>(&id_lower, saving_type)?;
    check_aliases_in::<crate::server::Server>(&id_lower, saving_type)?;
    check_aliases_in::<crate::extension::ExtensionManifest>(&id_lower, saving_type)?;
    check_aliases_in::<crate::fleet::Fleet>(&id_lower, saving_type)?;

    Ok(())
}

pub fn save<T: ConfigEntity>(entity: &T) -> Result<()> {
    identifier::validate_component_id(entity.id())?;
    check_id_collision(entity.id(), T::entity_type())?;

    let path = T::config_path(entity.id())?;
    local_files::ensure_app_dirs()?;
    // Ensure parent directory exists (supports directory-based entities like
    // projects and extensions where config_path is {dir}/{id}/{id}.json)
    if let Some(parent) = path.parent() {
        local_files::local().ensure_dir(parent)?;
    }
    let content = to_string_pretty(entity)?;
    local_files::local().write(&path, &content)?;
    Ok(())
}

/// Internal: create a single entity from a constructed struct.
/// Validates ID, checks for existence, runs entity-specific validation, then saves.
pub(crate) fn create_single<T: ConfigEntity>(entity: T) -> Result<CreateResult<T>> {
    identifier::validate_component_id(entity.id())?;
    entity.validate()?;

    if exists::<T>(entity.id()) {
        return Err(Error::validation_invalid_argument(
            format!("{}.id", T::entity_type()),
            format!("{} '{}' already exists", T::entity_type(), entity.id()),
            Some(entity.id().to_string()),
            None,
        ));
    }

    check_id_collision(entity.id(), T::entity_type())?;

    let path = T::config_path(entity.id())?;
    local_files::ensure_app_dirs()?;
    if let Some(parent) = path.parent() {
        local_files::local().ensure_dir(parent)?;
    }
    let content = to_string_pretty(&entity)?;
    local_files::local().write(&path, &content)?;

    Ok(CreateResult {
        id: entity.id().to_string(),
        entity,
    })
}

/// Internal: create a single entity from JSON string.
pub(crate) fn create_single_from_json<T: ConfigEntity>(json_spec: &str) -> Result<CreateResult<T>> {
    let value: serde_json::Value = from_str(json_spec)?;

    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument("id", "Missing required field: id", None, None)
        })?
        .to_string();

    let mut entity: T = serde_json::from_value(value)
        .map_err(|e| Error::validation_invalid_argument("json", e.to_string(), None, None))?;
    entity.set_id(id);

    create_single(entity)
}

/// Unified create that auto-detects single vs bulk operations.
/// Array input triggers batch create, object input triggers single create.
pub fn create<T: ConfigEntity>(
    json_spec: &str,
    skip_existing: bool,
) -> Result<CreateOutput<T>> {
    let raw = read_json_spec_to_string(json_spec)?;

    if is_json_array(&raw) {
        return Ok(CreateOutput::Bulk(create_batch::<T>(&raw, skip_existing)?));
    }

    Ok(CreateOutput::Single(create_single_from_json::<T>(&raw)?))
}

pub fn delete<T: ConfigEntity>(id: &str) -> Result<()> {
    let path = T::config_path(id)?;
    if !path.exists() {
        let suggestions = find_similar_ids::<T>(id);
        return Err(T::not_found_error(id.to_string(), suggestions));
    }
    local_files::local().delete(&path)?;

    // For directory-based entities, clean up the parent directory if it's
    // now empty (e.g., projects/{id}/ after removing {id}.json).
    if let Some(parent) = path.parent() {
        if parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == id)
        {
            if let Ok(mut entries) = std::fs::read_dir(parent) {
                if entries.next().is_none() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
    }

    Ok(())
}

pub fn exists<T: ConfigEntity>(id: &str) -> bool {
    T::config_path(id).map(|p| p.exists()).unwrap_or(false)
}

pub fn list_ids<T: ConfigEntity>() -> Result<Vec<String>> {
    let dir = T::config_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let entries = local_files::local().list(&dir)?;
    let mut ids: Vec<String> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| e.path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    ids.sort();
    Ok(ids)
}
