//! generic_json_operations — extracted from config.rs.

use crate::engine::identifier;
use crate::engine::local_files::{self, FileSystem};
use crate::engine::text::levenshtein;
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
use super::save;
use super::from_str;
use super::read_json_spec_to_string;
use super::delete;
use super::exists;
use super::load;
use super::list;
use super::ConfigEntity;


/// Batch create entities from JSON. Parses JSON array (or single object),
/// validates each entity, and saves. Supports skip_existing flag.
pub fn create_batch<T: ConfigEntity>(
    spec: &str,
    skip_existing: bool,
) -> Result<BatchResult> {
    let value: serde_json::Value = from_str(spec)?;
    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().expect("is_array() returned true").clone()
    } else {
        vec![value]
    };

    let mut summary = BatchResult::new();

    for item in items {
        let id = match item.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                summary.record_error(
                    "unknown".to_string(),
                    "Missing required field: id".to_string(),
                );
                continue;
            }
        };

        if let Err(e) = identifier::validate_component_id(&id) {
            summary.record_error(id, e.message.clone());
            continue;
        }

        let mut entity: T = match serde_json::from_value(item.clone()) {
            Ok(e) => e,
            Err(e) => {
                summary.record_error(id, format!("Parse error: {}", e));
                continue;
            }
        };
        entity.set_id(id.clone());

        // Entity-specific validation
        if let Err(e) = entity.validate() {
            summary.record_error(id, e.message.clone());
            continue;
        }

        if exists::<T>(&id) {
            if skip_existing {
                summary.record_skipped(id);
            } else {
                summary.record_error(
                    id.clone(),
                    format!("{} '{}' already exists", T::entity_type(), id),
                );
            }
            continue;
        }

        if let Err(e) = save(&entity) {
            summary.record_error(id, e.message.clone());
            continue;
        }

        summary.record_created(id);
    }

    Ok(summary)
}

pub fn merge_from_json<T: ConfigEntity>(
    id: Option<&str>,
    json_spec: &str,
    replace_fields: &[String],
) -> Result<MergeResult> {
    let raw = read_json_spec_to_string(json_spec)?;
    let mut parsed: serde_json::Value = from_str(&raw)?;

    let effective_id = id
        .map(String::from)
        .or_else(|| parsed.get("id").and_then(|v| v.as_str()).map(String::from))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                format!(
                    "Provide {} ID as argument or in JSON body",
                    T::entity_type()
                ),
                None,
                None,
            )
        })?;

    if let Some(obj) = parsed.as_object_mut() {
        obj.remove("id");
    }

    let mut entity = load::<T>(&effective_id)?;
    let result = merge_config(&mut entity, parsed, replace_fields)?;
    entity.set_id(effective_id.clone());
    save(&entity)?;

    Ok(MergeResult {
        id: effective_id,
        updated_fields: result.updated_fields,
    })
}

pub fn merge_batch_from_json<T: ConfigEntity>(raw_json: &str) -> Result<BatchResult> {
    let value: serde_json::Value = from_str(raw_json)?;

    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().expect("is_array() returned true").clone()
    } else {
        vec![value]
    };

    let mut result = BatchResult::new();

    for item in items {
        let id = match item.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                result.record_error(
                    "unknown".to_string(),
                    "Missing required field: id".to_string(),
                );
                continue;
            }
        };

        let mut patch = item.clone();
        if let Some(obj) = patch.as_object_mut() {
            obj.remove("id");
        }

        match load::<T>(&id) {
            Ok(mut entity) => match merge_config(&mut entity, patch, &[]) {
                Ok(_) => {
                    entity.set_id(id.clone());
                    if let Err(e) = save(&entity) {
                        result.record_error(id, e.message.clone());
                    } else {
                        result.record_updated(id);
                    }
                }
                Err(e) => {
                    result.record_error(id, e.message.clone());
                }
            },
            Err(e) => {
                result.record_error(id, format!("{} not found", T::entity_type()));
                let _ = e; // Suppress unused warning
            }
        }
    }

    Ok(result)
}

pub fn remove_from_json<T: ConfigEntity>(
    id: Option<&str>,
    json_spec: &str,
) -> Result<RemoveResult> {
    let raw = read_json_spec_to_string(json_spec)?;
    let mut parsed: serde_json::Value = from_str(&raw)?;

    let effective_id = id
        .map(String::from)
        .or_else(|| parsed.get("id").and_then(|v| v.as_str()).map(String::from))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                format!(
                    "Provide {} ID as argument or in JSON body",
                    T::entity_type()
                ),
                None,
                None,
            )
        })?;

    if let Some(obj) = parsed.as_object_mut() {
        obj.remove("id");
    }

    let mut entity = load::<T>(&effective_id)?;
    let result = remove_config(&mut entity, parsed)?;
    save(&entity)?;

    Ok(RemoveResult {
        id: effective_id,
        removed_from: result.removed_from,
    })
}

pub fn rename<T: ConfigEntity>(id: &str, new_id: &str) -> Result<()> {
    let new_id = new_id.to_lowercase();
    identifier::validate_component_id(&new_id)?;

    if new_id == id {
        return Ok(());
    }

    let old_path = T::config_path(id)?;
    let new_path = T::config_path(&new_id)?;

    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            format!("{}.id", T::entity_type()),
            format!(
                "Cannot rename {} '{}' to '{}': destination already exists",
                T::entity_type(),
                id,
                new_id
            ),
            Some(new_id.clone()),
            None,
        ));
    }

    // Check cross-entity name collision
    check_id_collision(&new_id, T::entity_type())?;

    let mut entity: T = load(id)?;
    entity.set_id(new_id.clone());

    local_files::ensure_app_dirs()?;

    // For directory-based entities (where the config file lives inside a
    // named directory, e.g. projects/{id}/{id}.json), rename the directory
    // first, then save the updated entity into the new location.
    let old_parent = old_path.parent().map(|p| p.to_path_buf());
    let new_parent = new_path.parent().map(|p| p.to_path_buf());
    let is_directory_based = old_parent.as_ref().is_some_and(|p| {
        p.file_name()
            .is_some_and(|name| name.to_string_lossy() == id)
    });

    if is_directory_based {
        if let (Some(old_dir), Some(new_dir)) = (&old_parent, &new_parent) {
            std::fs::rename(old_dir, new_dir).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("rename {} directory", T::entity_type())),
                )
            })?;
            // Rename the JSON file inside the new directory
            let old_json_in_new_dir = new_dir.join(format!("{}.json", id));
            if old_json_in_new_dir.exists() {
                std::fs::rename(&old_json_in_new_dir, &new_path).map_err(|e| {
                    Error::internal_io(
                        e.to_string(),
                        Some(format!("rename {} config file", T::entity_type())),
                    )
                })?;
            }
        }
    } else {
        std::fs::rename(&old_path, &new_path).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("rename {}", T::entity_type())))
        })?;
    }

    if let Err(error) = save(&entity) {
        // Rollback: move back
        if is_directory_based {
            if let (Some(old_dir), Some(new_dir)) = (&old_parent, &new_parent) {
                let _ = std::fs::rename(new_dir, old_dir);
            }
        } else {
            let _ = std::fs::rename(&new_path, &old_path);
        }
        return Err(error);
    }

    Ok(())
}

/// Delete an entity with referential integrity check.
///
/// Verifies the entity exists, checks for dependents via the trait hook,
/// and only deletes if nothing references it.
pub fn delete_safe<T: ConfigEntity>(id: &str) -> Result<()> {
    if !exists::<T>(id) {
        let suggestions = find_similar_ids::<T>(id);
        return Err(T::not_found_error(id.to_string(), suggestions));
    }

    let deps = T::dependents(id)?;
    if !deps.is_empty() {
        return Err(Error::validation_invalid_argument(
            T::ENTITY_TYPE,
            format!(
                "{} '{}' is in use by: {}. Remove references first.",
                T::ENTITY_TYPE,
                id,
                deps.join(", ")
            ),
            Some(id.to_string()),
            Some(deps),
        ));
    }

    delete::<T>(id)
}

/// Find entity IDs similar to the given target.
/// Uses prefix matching, suffix matching, and Levenshtein distance.
/// Returns up to 3 matches prioritized by match quality.
pub fn find_similar_ids<T: ConfigEntity>(target: &str) -> Vec<String> {
    let entities = match list::<T>() {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let target_lower = target.to_lowercase();
    let mut matches: Vec<(String, usize)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for entity in &entities {
        let id = entity.id().to_string();
        // Collect the ID and all aliases as candidates
        let mut candidates = vec![id.clone()];
        candidates.extend(entity.aliases().iter().cloned());

        for candidate in &candidates {
            let candidate_lower = candidate.to_lowercase();

            let priority =
                if candidate_lower.starts_with(&target_lower) && candidate_lower != target_lower {
                    Some(0) // Prefix match
                } else if candidate_lower.ends_with(&target_lower) {
                    Some(1) // Suffix match
                } else {
                    let dist = levenshtein(&target_lower, &candidate_lower);
                    if dist <= 3 && dist > 0 {
                        Some(dist + 10) // Fuzzy match
                    } else {
                        None
                    }
                };

            if let Some(p) = priority {
                // Show the real ID (with alias hint if matched via alias)
                let display = if candidate != &id {
                    format!("{} (alias: {})", id, candidate)
                } else {
                    id.clone()
                };
                if seen.insert(display.clone()) {
                    matches.push((display, p));
                }
            }
        }
    }

    matches.sort_by_key(|(_, priority)| *priority);
    matches.into_iter().take(3).map(|(id, _)| id).collect()
}
