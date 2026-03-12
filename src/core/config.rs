use crate::error::Error;
use crate::engine::identifier;
use crate::engine::text::levenshtein;
use crate::local_files::{self, FileSystem};
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::paths;
use crate::Result;
use heck::ToSnakeCase;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

// ============================================================================
// JSON Parsing Utilities (internal)
// ============================================================================

/// Parse JSON string into typed value.
pub(crate) fn from_str<T: DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse json".to_string()), None))
}

/// Serialize value to pretty-printed JSON string.
pub(crate) fn to_string_pretty<T: Serialize>(data: &T) -> Result<String> {
    serde_json::to_string_pretty(data)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))
}

/// Serialize value to compact JSON string with proper error handling.
pub fn to_json_string<T: Serialize>(data: &T) -> Result<String> {
    serde_json::to_string(data)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))
}

/// Serialize an entity to JSON and inject an `id` field.
///
/// Many entities use `#[serde(skip_serializing)]` on their `id` field, but
/// `create_single_from_json()` requires the id to be present. This helper
/// serializes the entity, injects the id, then returns a compact JSON string.
pub fn serialize_with_id<T: Serialize>(entity: &T, id: &str) -> Result<String> {
    let mut value = serde_json::to_value(entity)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize entity".to_string())))?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    }
    to_json_string(&value)
}

/// Read JSON spec from string, file (@path), or stdin (-).
pub fn read_json_spec_to_string(spec: &str) -> Result<String> {
    use std::io::IsTerminal;

    if spec.trim() == "-" {
        let mut buf = String::new();
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err(Error::validation_invalid_argument(
                "json",
                "Cannot read JSON from stdin when stdin is a TTY",
                None,
                None,
            ));
        }
        stdin
            .read_to_string(&mut buf)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read stdin".to_string())))?;
        return Ok(buf);
    }

    if let Some(path) = spec.strip_prefix('@') {
        if path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "json",
                "Invalid JSON spec '@' (missing file path)",
                None,
                None,
            ));
        }

        return local_files::local().read(Path::new(path));
    }

    Ok(spec.to_string())
}

/// Detect if input is JSON object (starts with '{').
pub(crate) fn is_json_input(input: &str) -> bool {
    input.trim_start().starts_with('{')
}

/// Detect if input is JSON array (starts with '[').
pub(crate) fn is_json_array(input: &str) -> bool {
    input.trim_start().starts_with('[')
}

// ============================================================================
// JSON Pointer Operations (internal)
// ============================================================================

pub fn set_json_pointer(root: &mut Value, pointer: &str, new_value: Value) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        *root = new_value;
        return Ok(());
    };

    let parent = ensure_pointer_container(root, &parent_ptr)?;
    set_child(parent, &token, new_value)
}

/// Remove the value at a JSON pointer path.
pub fn remove_json_pointer(root: &mut Value, pointer: &str) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Cannot remove root element",
            None,
            None,
        ));
    };

    let parent = navigate_pointer(root, &parent_ptr)?;
    remove_child(parent, &token)
}

/// Navigate to the value at a JSON pointer without creating intermediate objects.
/// Returns an error if any segment along the path is missing.
fn navigate_pointer<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
    if pointer.is_empty() {
        return Ok(root);
    }

    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape_token).collect();
    let mut current = root;

    for token in &tokens {
        current = match current {
            Value::Object(map) => map.get_mut(token.as_str()).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "pointer",
                    format!("Key '{}' not found", token),
                    None,
                    None,
                )
            })?,
            Value::Array(arr) => {
                let index = parse_array_index(token)?;
                let len = arr.len();
                if index >= len {
                    return Err(Error::validation_invalid_argument(
                        "pointer",
                        format!("Array index {} out of bounds (length {})", index, len),
                        None,
                        None,
                    ));
                }
                &mut arr[index]
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Cannot navigate through non-object at path: {}", pointer),
                    None,
                    None,
                ))
            }
        };
    }

    Ok(current)
}

fn remove_child(parent: &mut Value, token: &str) -> Result<()> {
    match parent {
        Value::Object(map) => {
            if map.remove(token).is_none() {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Key '{}' not found", token),
                    None,
                    None,
                ));
            }
            Ok(())
        }
        Value::Array(arr) => {
            let index = parse_array_index(token)?;
            if index >= arr.len() {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Array index {} out of bounds (length {})", index, arr.len()),
                    None,
                    None,
                ));
            }
            arr.remove(index);
            Ok(())
        }
        _ => Err(Error::validation_invalid_argument(
            "pointer",
            "Cannot remove from non-container type",
            None,
            None,
        )),
    }
}

fn normalize_pointer(pointer: &str) -> Result<String> {
    if pointer.is_empty() {
        return Ok(String::new());
    }

    if pointer == "/" {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Invalid JSON pointer '/'",
            None,
            None,
        ));
    }

    if !pointer.starts_with('/') {
        return Err(Error::validation_invalid_argument(
            "pointer",
            format!("JSON pointer must start with '/': {}", pointer),
            None,
            None,
        ));
    }

    Ok(pointer.to_string())
}

fn split_parent_pointer(pointer: &str) -> Option<(String, String)> {
    if pointer.is_empty() {
        return None;
    }

    let mut parts = pointer.rsplitn(2, '/');
    let token = parts.next()?.to_string();
    let parent = parts.next().unwrap_or("");

    let parent_ptr = if parent.is_empty() {
        String::new()
    } else {
        parent.to_string()
    };

    Some((parent_ptr, unescape_token(&token)))
}

fn ensure_pointer_container<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
    if pointer.is_empty() {
        return Ok(root);
    }

    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape_token).collect();

    let mut current = root;

    for token in tokens {
        let next = match current {
            Value::Object(map) => map
                .entry(token)
                .or_insert_with(|| Value::Object(serde_json::Map::new())),
            Value::Null => {
                *current = Value::Object(serde_json::Map::new());
                if let Value::Object(map) = current {
                    map.entry(token)
                        .or_insert_with(|| Value::Object(serde_json::Map::new()))
                } else {
                    unreachable!()
                }
            }
            Value::Array(arr) => {
                let index = parse_array_index(&token)?;
                if index >= arr.len() {
                    return Err(Error::config_invalid_value(
                        pointer,
                        None,
                        "Array index out of bounds while creating path",
                    ));
                }
                &mut arr[index]
            }
            _ => {
                return Err(Error::config_invalid_value(
                    pointer,
                    Some(value_type_name(current).to_string()),
                    "Expected object/array at pointer",
                ))
            }
        };

        current = next;
    }

    Ok(current)
}

fn set_child(parent: &mut Value, token: &str, value: Value) -> Result<()> {
    match parent {
        Value::Object(map) => {
            map.insert(token.to_string(), value);
            Ok(())
        }
        Value::Array(arr) => {
            let index = parse_array_index(token)?;
            if index >= arr.len() {
                return Err(Error::config_invalid_value(
                    "arrayIndex",
                    Some(index.to_string()),
                    "Array index out of bounds",
                ));
            }
            arr[index] = value;
            Ok(())
        }
        _ => Err(Error::config_invalid_value(
            "jsonPointer",
            Some(value_type_name(parent).to_string()),
            "Cannot set child on non-container",
        )),
    }
}

fn parse_array_index(token: &str) -> Result<usize> {
    token.parse::<usize>().map_err(|_| {
        Error::validation_invalid_argument(
            "arrayIndex",
            "Invalid array index token",
            Some(token.to_string()),
            None,
        )
    })
}

fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ============================================================================
// Config Merge/Remove Operations (internal)
// ============================================================================

/// Normalize JSON object keys to snake_case recursively.
/// Allows callers to use camelCase, PascalCase, or snake_case interchangeably.
fn normalize_keys_to_snake_case(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let normalized: Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k.to_snake_case(), normalize_keys_to_snake_case(v)))
                .collect();
            Value::Object(normalized)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(normalize_keys_to_snake_case).collect())
        }
        other => other,
    }
}

/// Internal result from merge_config (no ID, caller adds it).
#[derive(Debug)]
pub(crate) struct MergeFields {
    pub updated_fields: Vec<String>,
}

/// Merge a JSON patch into any serializable config type.
pub(crate) fn merge_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    patch: Value,
    replace_fields: &[String],
) -> Result<MergeFields> {
    // Normalize keys to snake_case (accepts camelCase, PascalCase, etc.)
    let patch = normalize_keys_to_snake_case(patch);

    let patch_obj = match &patch {
        Value::Object(obj) => obj,
        _ => {
            return Err(Error::validation_invalid_argument(
                "merge",
                "Merge patch must be a JSON object",
                None,
                None,
            ))
        }
    };

    let updated_fields: Vec<String> = patch_obj.keys().cloned().collect();

    if updated_fields.is_empty() {
        return Err(Error::validation_invalid_argument(
            "merge",
            "Merge patch cannot be empty",
            None,
            None,
        ));
    }

    // Detect unknown fields by round-tripping through the typed struct.
    // After deep-merging the patch into the serialized base, deserialize back
    // into T. Serde silently drops unknown keys. We detect this by comparing
    // the merged JSON against the re-serialized struct output.
    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    // Snapshot patch values before merge (for zero-value detection)
    let patch_values: Map<String, Value> = patch.as_object().cloned().unwrap_or_default();

    deep_merge(&mut base, patch, replace_fields, String::new());

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("merge config".to_string()), None))?;

    // Re-serialize and check which patch keys survived the round-trip.
    // Fields with skip_serializing_if may vanish when set to zero values
    // (empty vec, None, false), so we only flag keys whose patch value was
    // non-trivial but still disappeared — those are truly unknown fields.
    let after_roundtrip = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;
    let surviving_keys: std::collections::HashSet<String> = after_roundtrip
        .as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    let dropped: Vec<&String> = updated_fields
        .iter()
        .filter(|key| {
            if surviving_keys.contains(key.as_str()) {
                return false; // Key survived — it's known
            }
            // Key disappeared. Check if the patch value was a "zero value"
            // that skip_serializing_if would legitimately omit.
            match patch_values.get(key.as_str()) {
                None => false, // Shouldn't happen, but be safe
                Some(val) => !is_serialization_zero(val),
            }
        })
        .collect();

    if !dropped.is_empty() {
        let field_list = dropped
            .iter()
            .map(|k| format!("'{}'", k))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::validation_invalid_argument(
            "merge",
            format!(
                "Unknown field(s): {}. Check field names with the entity's config schema.",
                field_list
            ),
            None,
            None,
        ));
    }

    Ok(MergeFields { updated_fields })
}

/// Internal result from remove_config (no ID, caller adds it).
pub(crate) struct RemoveFields {
    pub removed_from: Vec<String>,
}

/// Remove items from arrays in any serializable config type.
pub(crate) fn remove_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    spec: Value,
) -> Result<RemoveFields> {
    let spec_obj = match &spec {
        Value::Object(obj) => obj,
        _ => {
            return Err(Error::validation_invalid_argument(
                "remove",
                "Remove spec must be a JSON object",
                None,
                None,
            ))
        }
    };

    let fields: Vec<String> = spec_obj.keys().cloned().collect();

    if fields.is_empty() {
        return Err(Error::validation_invalid_argument(
            "remove",
            "Remove spec cannot be empty",
            None,
            None,
        ));
    }

    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    let mut removed_from = Vec::new();
    deep_remove(&mut base, spec, &mut removed_from, String::new());

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("remove config".to_string()), None))?;

    Ok(RemoveFields { removed_from })
}

fn deep_remove(base: &mut Value, spec: Value, removed_from: &mut Vec<String>, path: String) {
    match (base, spec) {
        (Value::Object(base_obj), Value::Object(spec_obj)) => {
            for (key, value) in spec_obj {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                if let Some(base_value) = base_obj.get_mut(&key) {
                    deep_remove(base_value, value, removed_from, field_path);
                }
            }
        }
        (Value::Array(base_arr), Value::Array(spec_arr)) => {
            let original_len = base_arr.len();
            base_arr.retain(|item| !spec_arr.contains(item));
            if base_arr.len() < original_len {
                removed_from.push(path);
            }
        }
        _ => {}
    }
}

/// Returns true if a JSON value is a "zero value" that skip_serializing_if
/// would legitimately omit (empty array, empty string, null, false, 0).
fn is_serialization_zero(val: &Value) -> bool {
    match val {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(arr) => arr.is_empty(),
        Value::Object(obj) => obj.is_empty(),
        _ => false,
    }
}

/// Collect top-level array field names from a JSON object.
/// Used by `set` commands to auto-replace arrays instead of merging.
pub fn collect_array_fields(value: &Value) -> Vec<String> {
    match value {
        Value::Object(obj) => obj
            .iter()
            .filter(|(_, v)| v.is_array())
            .map(|(k, _)| k.clone())
            .collect(),
        _ => vec![],
    }
}

fn should_replace(path: &str, replace_fields: &[String]) -> bool {
    replace_fields
        .iter()
        .any(|field| path == field || path.starts_with(&format!("{}.", field)))
}

fn deep_merge(base: &mut Value, patch: Value, replace_fields: &[String], path: String) {
    match (base, patch) {
        (Value::Object(base_obj), Value::Object(patch_obj)) => {
            for (key, value) in patch_obj {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                if value.is_null() {
                    base_obj.remove(&key);
                } else {
                    let entry = base_obj.entry(key).or_insert(Value::Null);
                    deep_merge(entry, value, replace_fields, field_path);
                }
            }
        }
        (Value::Array(base_arr), Value::Array(patch_arr)) => {
            if should_replace(&path, replace_fields) {
                *base_arr = patch_arr;
            } else {
                array_union(base_arr, patch_arr);
            }
        }
        (base, patch) => *base = patch,
    }
}

fn array_union(base: &mut Vec<Value>, patch: Vec<Value>) {
    for item in patch {
        if !base.contains(&item) {
            base.push(item);
        }
    }
}

// ============================================================================
// Bulk Input Parsing
// ============================================================================

/// Simple bulk input with just component IDs.
#[derive(Debug, Clone, Deserialize)]

pub(crate) struct BulkIdsInput {
    pub component_ids: Vec<String>,
}

/// Parse JSON spec into a BulkIdsInput.
pub(crate) fn parse_bulk_ids(json_spec: &str) -> Result<BulkIdsInput> {
    let raw = read_json_spec_to_string(json_spec)?;
    if let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) {
        return Ok(BulkIdsInput { component_ids: ids });
    }

    serde_json::from_str::<BulkIdsInput>(&raw)
        .map_err(|e| {
            Error::validation_invalid_json(
                e,
                Some("parse bulk IDs input".to_string()),
                Some(raw.chars().take(200).collect::<String>()),
            )
        })
        .map_err(|err| {
            err.with_hint(
                "Expected JSON array: '[\"component-a\",\"component-b\"]' OR JSON object: '{\"component_ids\":[\"component-a\",\"component-b\"]}'",
            )
        })
}

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

pub(crate) fn load<T: ConfigEntity>(id: &str) -> Result<T> {
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
fn resolve_alias<T: ConfigEntity>(alias: &str) -> Option<String> {
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

pub(crate) fn list<T: ConfigEntity>() -> Result<Vec<T>> {
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

pub(crate) fn check_id_collision(id: &str, saving_type: &str) -> Result<()> {
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
fn check_alias_collision_all(id: &str, saving_type: &str) -> Result<()> {
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

pub(crate) fn save<T: ConfigEntity>(entity: &T) -> Result<()> {
    identifier::validate_component_id(entity.id())?;
    check_id_collision(entity.id(), T::entity_type())?;

    let path = T::config_path(entity.id())?;
    local_files::ensure_app_dirs()?;
    let content = to_string_pretty(entity)?;
    local_files::local().write(&path, &content)?;
    Ok(())
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

    let path = T::config_path(entity.id())?;
    local_files::ensure_app_dirs()?;
    let content = to_string_pretty(&entity)?;
    local_files::local().write(&path, &content)?;

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
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| e.path.file_stem().map(|s| s.to_string_lossy().to_string()))
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
    std::fs::rename(&old_path, &new_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("rename {}", T::entity_type())))
    })?;

    if let Err(error) = save(&entity) {
        let _ = std::fs::rename(&new_path, &old_path);
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
            crate::engine::identifier::slugify_id(name, "name")
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test struct mimicking Component's skip_serializing_if patterns.
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    struct TestConfig {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub extensions: Option<std::collections::HashMap<String, serde_json::Value>>,
    }

    #[test]
    fn merge_config_rejects_unknown_fields() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"extension": "wordpress"});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let problem = err.details["problem"].as_str().unwrap_or("");
        assert!(
            problem.contains("Unknown field(s)"),
            "Expected unknown field error, got: {}",
            problem
        );
        assert!(
            problem.contains("'extension'"),
            "Expected 'extension' in error, got: {}",
            problem
        );
    }

    #[test]
    fn merge_config_accepts_known_fields() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"description": "hello"});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
        assert_eq!(config.description, Some("hello".to_string()));
    }

    #[test]
    fn merge_config_allows_zero_value_for_known_fields() {
        // Setting a known field to an empty/zero value should not be rejected,
        // even though skip_serializing_if will omit it from output.
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        // tags starts empty; patching with empty array is a valid zero-value
        // that skip_serializing_if would omit, but it's still a known field.
        let patch = serde_json::json!({"tags": []});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn merge_config_accepts_modules_plural() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"extensions": {"wordpress": {}}});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
        assert!(config.extensions.is_some());
    }

    #[test]
    fn parse_bulk_ids_accepts_json_array() {
        let parsed = parse_bulk_ids(r#"["api","web"]"#).unwrap();
        assert_eq!(parsed.component_ids, vec!["api", "web"]);
    }

    #[test]
    fn parse_bulk_ids_accepts_json_object() {
        let parsed = parse_bulk_ids(r#"{"component_ids":["api","web"]}"#).unwrap();
        assert_eq!(parsed.component_ids, vec!["api", "web"]);
    }

    #[test]
    fn parse_bulk_ids_invalid_input_has_shape_hint() {
        let err = parse_bulk_ids("api, web").unwrap_err();
        let hints = err.hints;
        assert!(
            hints
                .iter()
                .any(|h| h.message.contains("Expected JSON array")),
            "expected parse hint, got: {:?}",
            hints
        );
    }
}
