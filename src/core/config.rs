use crate::core::engine::identifier;
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::engine::text::levenshtein;
use crate::core::error::Error;
use crate::core::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::core::paths;
use crate::core::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

mod json_io;
mod json_ops;
mod json_pointer;
pub(crate) use json_io::{
    from_str, is_json_array, is_json_input, parse_bulk_ids, read_json_spec_to_string,
    to_string_pretty,
};
pub use json_io::{serialize_with_id, to_json_string};
pub use json_ops::collect_array_fields;
pub(crate) use json_ops::{merge_config, remove_config};
pub(crate) use json_pointer::value_type_name;
pub use json_pointer::{remove_json_pointer, set_json_pointer};

// ============================================================================
// Config Entity Trait
// ============================================================================

pub(crate) trait ConfigEntity: Serialize + DeserializeOwned {
    /// The entity type name (e.g., "project", "server", "component", "extension").
    const ENTITY_TYPE: &'static str;

    /// The directory name within the config root (e.g., "projects", "servers").
    const DIR_NAME: &'static str;

    // Required methods - only these need implementation
    fn id(&self) -> &str;
    fn set_id(&mut self, id: String);

    /// Entity-specific "not found" error. Required to preserve specific error codes.
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error;

    // Default implementations

    /// Returns the entity type name.
    fn entity_type() -> &'static str {
        Self::ENTITY_TYPE
    }

    /// Returns the config directory path.
    fn config_dir() -> Result<PathBuf> {
        Ok(paths::homeboy()?.join(Self::DIR_NAME))
    }

    /// Returns the config file path for a given ID.
    /// Default: `{dir}/{id}.json`. Override for non-standard paths.
    fn config_path(id: &str) -> Result<PathBuf> {
        Ok(Self::config_dir()?.join(format!("{}.json", id)))
    }

    /// Whether this entity accepts `{dir}/{id}.json` entries while listing.
    fn supports_flat_config_entries() -> bool {
        true
    }

    /// Entity-specific validation. Override to add custom validation rules.
    fn validate(&self) -> Result<()> {
        Ok(())
    }

    /// Returns the entity's aliases. Override to support alias-based lookup.
    fn aliases(&self) -> &[String] {
        &[]
    }

    /// Post-load hook called after deserializing from disk.
    /// `stored_json` is the raw JSON string from the config file, allowing
    /// implementations to determine which fields were explicitly set vs defaulted.
    /// Override to apply runtime config layering (e.g., portable config overlay).
    /// Default: no-op.
    fn post_load(&mut self, _stored_json: &str) {}

    /// Return IDs of entities that depend on this one (for safe delete).
    /// Override to check referential integrity before deletion.
    /// Default: no dependents.
    fn dependents(_id: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    /// Update references after rename. Called after the config file is renamed.
    /// Override to propagate ID changes to entities that reference this one.
    /// Default: no-op.
    fn on_rename(_old_id: &str, _new_id: &str) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct ConfigEntityMetadata {
    entity_type: &'static str,
    exists: fn(&str) -> bool,
    aliases: fn() -> Vec<(String, String)>,
}

fn entity_metadata<T: ConfigEntity>() -> ConfigEntityMetadata {
    ConfigEntityMetadata {
        entity_type: T::ENTITY_TYPE,
        exists: exists::<T>,
        aliases: alias_entries::<T>,
    }
}

fn config_entity_registry() -> [ConfigEntityMetadata; 6] {
    [
        entity_metadata::<crate::core::project::Project>(),
        entity_metadata::<crate::core::server::Server>(),
        entity_metadata::<crate::core::runner::Runner>(),
        entity_metadata::<crate::core::tunnel::ServiceTunnel>(),
        entity_metadata::<crate::core::extension::ExtensionManifest>(),
        entity_metadata::<crate::core::fleet::Fleet>(),
    ]
}

fn alias_entries<T: ConfigEntity>() -> Vec<(String, String)> {
    list::<T>()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|entity| {
            let entity_id = entity.id().to_string();
            let aliases = entity.aliases().to_vec();
            aliases
                .into_iter()
                .map(move |alias| (entity_id.clone(), alias))
        })
        .collect()
}

pub(crate) fn load<T: ConfigEntity>(id: &str) -> Result<T> {
    let path = T::config_path(id)?;
    if !path.exists() {
        let entities = list::<T>().unwrap_or_default();
        // Try alias resolution before giving up
        if let Some(real_id) = resolve_alias_in(id, &entities) {
            let alias_path = T::config_path(&real_id)?;
            let content = local_files::local().read(&alias_path)?;
            let mut entity: T = from_str(&content)?;
            entity.set_id(real_id);
            entity.post_load(&content);
            return Ok(entity);
        }
        let suggestions = find_similar_ids_in(id, &entities);
        return Err(T::not_found_error(id.to_string(), suggestions));
    }
    let content = local_files::local().read(&path)?;
    let mut entity: T = from_str(&content)?;
    entity.set_id(id.to_string());
    entity.post_load(&content);
    Ok(entity)
}

fn resolve_alias_in<T: ConfigEntity>(alias: &str, entities: &[T]) -> Option<String> {
    let alias_lower = alias.to_lowercase();
    for entity in entities {
        for a in entity.aliases() {
            if a.to_lowercase() == alias_lower {
                return Some(entity.id().to_string());
            }
        }
    }
    None
}

/// Resolve a directory entry to its config JSON path and entity ID.
///
/// Supports directory-backed entities (`{dir}/{id}/{id}.json`) and flat
/// entries (`{dir}/{id}.json`). Returns `None` for entries that are not
/// valid config entities (e.g. a directory missing its nested JSON, or a
/// non-JSON file). Shared by the `list` and `list_ids` discovery paths.
fn entry_path_and_id<T: ConfigEntity>(entry: &local_files::Entry) -> Option<(PathBuf, String)> {
    if entry.is_dir {
        // For directories (extension structure): look for {dir}/{dir}.json
        let dir_name = entry.path.file_name()?.to_string_lossy().to_string();
        let nested_json = entry.path.join(format!("{}.json", dir_name));
        if nested_json.exists() {
            Some((nested_json, dir_name))
        } else {
            None
        }
    } else if T::supports_flat_config_entries() && entry.is_json() {
        let id = entry.path.file_stem()?.to_string_lossy().to_string();
        Some((entry.path.clone(), id))
    } else {
        None
    }
}

pub(crate) fn list<T: ConfigEntity>() -> Result<Vec<T>> {
    let dir = T::config_dir()?;
    let entries = local_files::local().list(&dir)?;

    let mut items: Vec<T> = entries
        .into_iter()
        .filter_map(|e| {
            // Determine the path to the JSON file and the ID
            let (json_path, id) = entry_path_and_id::<T>(&e)?;

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

pub(crate) fn check_id_collision(id: &str, saving_type: &str) -> Result<()> {
    for metadata in config_entity_registry() {
        if metadata.entity_type == saving_type {
            continue;
        }

        if (metadata.exists)(id) {
            return Err(Error::config_id_collision(
                id,
                saving_type,
                metadata.entity_type,
            ));
        }
    }

    // Check if the ID collides with any existing alias
    check_alias_collision_all(id, saving_type)?;

    Ok(())
}

/// Check if a given ID or alias collides with any existing entity's aliases.
fn check_alias_collision_all(id: &str, saving_type: &str) -> Result<()> {
    let id_lower = id.to_lowercase();

    for metadata in config_entity_registry() {
        if metadata.entity_type == saving_type {
            continue;
        }

        for (entity_id, alias) in (metadata.aliases)() {
            if alias.to_lowercase() == id_lower {
                return Err(Error::config(format!(
                    "ID '{}' conflicts with an alias on {} '{}'",
                    id_lower, metadata.entity_type, entity_id
                )));
            }
        }
    }

    Ok(())
}

/// Serialize an entity and write it to its config path.
///
/// Ensures the app config dirs exist, creates the entity's parent directory
/// (supporting directory-based entities like projects and extensions where
/// `config_path` is `{dir}/{id}/{id}.json`), serializes the entity as pretty
/// JSON, and writes the file. Callers are responsible for validation and
/// collision checks before invoking this helper.
fn write_entity<T: ConfigEntity>(entity: &T) -> Result<()> {
    let path = T::config_path(entity.id())?;
    local_files::ensure_app_dirs()?;
    if let Some(parent) = path.parent() {
        local_files::local().ensure_dir(parent)?;
    }
    let content = to_string_pretty(entity)?;
    local_files::local().write(&path, &content)?;
    Ok(())
}

pub(crate) fn save<T: ConfigEntity>(entity: &T) -> Result<()> {
    identifier::validate_component_id(entity.id())?;
    check_id_collision(entity.id(), T::entity_type())?;

    write_entity(entity)
}

/// Internal: create a single entity from a constructed struct.
/// Validates ID, checks for existence, runs entity-specific validation, then saves.
fn create_single<T: ConfigEntity>(entity: T) -> Result<CreateResult<T>> {
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

    write_entity(&entity)?;

    Ok(CreateResult {
        id: entity.id().to_string(),
        entity,
    })
}

/// Internal: create a single entity from JSON string.
fn create_single_from_json<T: ConfigEntity>(json_spec: &str) -> Result<CreateResult<T>> {
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
pub(crate) fn create<T: ConfigEntity>(
    json_spec: &str,
    skip_existing: bool,
) -> Result<CreateOutput<T>> {
    let raw = read_json_spec_to_string(json_spec)?;

    if is_json_array(&raw) {
        return Ok(CreateOutput::Bulk(create_batch::<T>(&raw, skip_existing)?));
    }

    Ok(CreateOutput::Single(create_single_from_json::<T>(&raw)?))
}

pub(crate) fn delete<T: ConfigEntity>(id: &str) -> Result<()> {
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

pub(crate) fn exists<T: ConfigEntity>(id: &str) -> bool {
    T::config_path(id).map(|p| p.exists()).unwrap_or(false)
}

pub(crate) fn list_ids<T: ConfigEntity>() -> Result<Vec<String>> {
    let dir = T::config_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let entries = local_files::local().list(&dir)?;
    let mut ids: Vec<String> = entries
        .into_iter()
        .filter_map(|e| entry_path_and_id::<T>(&e).map(|(_, id)| id))
        .collect();
    ids.sort();
    Ok(ids)
}

// ============================================================================
// Merge Operations
// ============================================================================

/// Unified merge that auto-detects single vs bulk operations.
/// Array input triggers batch merge, object input triggers single merge.
pub(crate) fn merge<T: ConfigEntity>(
    id: Option<&str>,
    json_spec: &str,
    replace_fields: &[String],
) -> Result<MergeOutput> {
    let raw = read_json_spec_to_string(json_spec)?;

    if is_json_array(&raw) {
        return Ok(MergeOutput::Bulk(merge_batch_from_json::<T>(&raw)?));
    }

    Ok(MergeOutput::Single(merge_from_json::<T>(
        id,
        &raw,
        replace_fields,
    )?))
}

// ============================================================================
// Generic JSON Operations
// ============================================================================

/// Batch create entities from JSON. Parses JSON array (or single object),
/// validates each entity, and saves. Supports skip_existing flag.
pub(crate) fn create_batch<T: ConfigEntity>(
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

pub(crate) fn merge_from_json<T: ConfigEntity>(
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

pub(crate) fn merge_batch_from_json<T: ConfigEntity>(raw_json: &str) -> Result<BatchResult> {
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

pub(crate) fn remove_from_json<T: ConfigEntity>(
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

pub(crate) fn rename<T: ConfigEntity>(id: &str, new_id: &str) -> Result<()> {
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
pub(crate) fn delete_safe<T: ConfigEntity>(id: &str) -> Result<()> {
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
pub(crate) fn find_similar_ids<T: ConfigEntity>(target: &str) -> Vec<String> {
    let entities = match list::<T>() {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    find_similar_ids_in(target, &entities)
}

fn find_similar_ids_in<T: ConfigEntity>(target: &str, entities: &[T]) -> Vec<String> {
    let target_lower = target.to_lowercase();
    let mut matches: Vec<(String, usize)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for entity in entities {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Serialize)]
    struct TestEntity {
        id: String,
        aliases: Vec<String>,
    }

    impl ConfigEntity for TestEntity {
        const ENTITY_TYPE: &'static str = "test";
        const DIR_NAME: &'static str = "tests";

        fn id(&self) -> &str {
            &self.id
        }

        fn set_id(&mut self, id: String) {
            self.id = id;
        }

        fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
            Error::entity_not_found(
                crate::core::error::ErrorCode::ConfigMissingKey,
                "Test",
                id,
                suggestions,
            )
        }

        fn aliases(&self) -> &[String] {
            &self.aliases
        }
    }

    #[test]
    fn miss_helpers_reuse_loaded_entities_for_aliases_and_suggestions() {
        let entities = vec![
            TestEntity {
                id: "alpha-project".to_string(),
                aliases: vec!["prod".to_string()],
            },
            TestEntity {
                id: "beta-project".to_string(),
                aliases: vec!["staging".to_string()],
            },
        ];

        assert_eq!(
            resolve_alias_in("PROD", &entities),
            Some("alpha-project".to_string())
        );
        assert_eq!(
            find_similar_ids_in("stag", &entities),
            vec!["beta-project (alias: staging)".to_string()]
        );
    }
}

// ============================================================================
// Entity CRUD Macro
// ============================================================================

/// Generate standard CRUD wrapper functions for a `ConfigEntity` type.
///
/// The base invocation generates 9 universal wrappers that every entity needs:
/// `load`, `list`, `save`, `delete`, `exists`, `remove_from_json`, `create`,
/// `rename`, `delete_safe`.
///
/// `rename` calls `config::rename`, then the entity's `on_rename` hook
/// (for updating references in other entities), then reloads.
///
/// `delete_safe` checks for dependents via the entity's `dependents` hook
/// before deleting. Use `delete` for unconditional removal.
///
/// Optional features add extra wrappers:
/// - `list_ids` — generates `list_ids() -> Result<Vec<String>>`
/// - `merge` — generates the standard `merge()` one-liner (entities with
///   custom merge logic should omit this and implement their own)
/// - `slugify_id` — generates `slugify_id(name) -> Result<String>`
///
/// # Examples
///
/// ```ignore
/// // All features:
/// entity_crud!(Project; list_ids, merge, slugify_id);
///
/// // Subset:
/// entity_crud!(Server; merge);
///
/// // Base only (entity has custom merge):
/// entity_crud!(Component; list_ids);
/// ```
macro_rules! entity_crud {
    // Entry point: split base from optional features
    ($Entity:ty $(; $($feature:ident),+ )?) => {
        // --- Universal wrappers (always generated) ---

        pub fn load(id: &str) -> Result<$Entity> {
            config::load::<$Entity>(id)
        }

        pub fn list() -> Result<Vec<$Entity>> {
            config::list::<$Entity>()
        }

        pub fn save(entity: &$Entity) -> Result<()> {
            config::save(entity)
        }

        pub fn delete(id: &str) -> Result<()> {
            config::delete::<$Entity>(id)
        }

        pub fn exists(id: &str) -> bool {
            config::exists::<$Entity>(id)
        }

        pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
            config::remove_from_json::<$Entity>(id, json_spec)
        }

        pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<$Entity>> {
            config::create::<$Entity>(json_spec, skip_existing)
        }

        pub fn rename(id: &str, new_id: &str) -> Result<$Entity> {
            config::rename::<$Entity>(id, new_id)?;
            let resolved_id = new_id.to_lowercase();
            <$Entity as config::ConfigEntity>::on_rename(id, &resolved_id)?;
            load(&resolved_id)
        }

        pub fn delete_safe(id: &str) -> Result<()> {
            config::delete_safe::<$Entity>(id)
        }

        // --- Optional features ---
        $( $(entity_crud!(@feature $Entity, $feature);)+ )?
    };

    // Feature: list_ids
    (@feature $Entity:ty, list_ids) => {
        pub fn list_ids() -> Result<Vec<String>> {
            config::list_ids::<$Entity>()
        }
    };

    // Feature: merge (standard one-liner)
    (@feature $Entity:ty, merge) => {
        pub fn merge(
            id: Option<&str>,
            json_spec: &str,
            replace_fields: &[String],
        ) -> Result<MergeOutput> {
            config::merge::<$Entity>(id, json_spec, replace_fields)
        }
    };

    // Feature: slugify_id
    (@feature $Entity:ty, slugify_id) => {
        pub fn slugify_id(name: &str) -> Result<String> {
            crate::core::engine::identifier::slugify_id(name, "name")
        }
    };
}
