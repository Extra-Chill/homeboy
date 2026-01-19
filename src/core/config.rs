use crate::error::Error;
use crate::local_files::{self, FileSystem};
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::paths;
use crate::slugify;
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

pub(crate) fn set_json_pointer(root: &mut Value, pointer: &str, new_value: Value) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        *root = new_value;
        return Ok(());
    };

    let parent = ensure_pointer_container(root, &parent_ptr)?;
    set_child(parent, &token, new_value)
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

    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    deep_merge(&mut base, patch, replace_fields, String::new());

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("merge config".to_string()), None))?;

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
    serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk IDs input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })
}

// ============================================================================
// Config Entity Trait
// ============================================================================

pub(crate) trait ConfigEntity: Serialize + DeserializeOwned {
    /// The entity type name (e.g., "project", "server", "component", "module").
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
}

pub(crate) fn load<T: ConfigEntity>(id: &str) -> Result<T> {
    let path = T::config_path(id)?;
    if !path.exists() {
        let suggestions = find_similar_ids::<T>(id);
        return Err(T::not_found_error(id.to_string(), suggestions));
    }
    let content = local_files::local().read(&path)?;
    let mut entity: T = from_str(&content)?;
    entity.set_id(id.to_string());
    Ok(entity)
}

pub(crate) fn list<T: ConfigEntity>() -> Result<Vec<T>> {
    let dir = T::config_dir()?;
    let entries = local_files::local().list(&dir)?;

    let mut items: Vec<T> = entries
        .into_iter()
        .filter_map(|e| {
            // Determine the path to the JSON file and the ID
            let (json_path, id) = if e.is_dir {
                // For directories (module structure): look for {dir}/{dir}.json
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
                    eprintln!("[config] Warning: failed to read {}: {}", json_path.display(), err);
                    return None;
                }
            };
            let mut entity: T = match from_str(&content) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("[config] Warning: failed to parse {}: {}", json_path.display(), err);
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

fn check_id_collision(id: &str, saving_type: &str) -> Result<()> {
    let entity_types = [
        ("project", paths::projects()),
        ("server", paths::servers()),
        ("component", paths::components()),
    ];

    for (entity_type, dir_result) in entity_types {
        if entity_type == saving_type {
            continue;
        }
        if let Ok(dir) = dir_result {
            let path = dir.join(format!("{}.json", id));
            if path.exists() {
                return Err(Error::config_id_collision(id, saving_type, entity_type));
            }
        }
    }
    Ok(())
}

pub(crate) fn save<T: ConfigEntity>(entity: &T) -> Result<()> {
    slugify::validate_component_id(entity.id())?;
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
    slugify::validate_component_id(entity.id())?;
    entity.validate()?;

    if exists::<T>(entity.id()) {
        return Err(Error::validation_invalid_argument(
            &format!("{}.id", T::entity_type()),
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

        if let Err(e) = slugify::validate_component_id(&id) {
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
    slugify::validate_component_id(&new_id)?;

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
            Some(new_id),
            None,
        ));
    }

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

// ============================================================================
// Fuzzy Matching
// ============================================================================

/// Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row: Vec<usize> = vec![0; b_len + 1];

    for (i, a_char) in a_chars.iter().enumerate() {
        curr_row[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let cost = if a_char == b_char { 0 } else { 1 };
            curr_row[j + 1] = (prev_row[j + 1] + 1)
                .min(curr_row[j] + 1)
                .min(prev_row[j] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

/// Find entity IDs similar to the given target.
/// Uses prefix matching, suffix matching, and Levenshtein distance.
/// Returns up to 3 matches prioritized by match quality.
pub(crate) fn find_similar_ids<T: ConfigEntity>(target: &str) -> Vec<String> {
    let existing = match list_ids::<T>() {
        Ok(ids) => ids,
        Err(_) => return vec![],
    };

    let target_lower = target.to_lowercase();
    let mut matches: Vec<(String, usize)> = Vec::new();

    for id in existing {
        let id_lower = id.to_lowercase();

        // Priority 0: Prefix match (target is prefix of existing)
        if id_lower.starts_with(&target_lower) && id_lower != target_lower {
            matches.push((id, 0));
            continue;
        }

        // Priority 1: Suffix match (target is suffix of existing)
        if id_lower.ends_with(&target_lower) {
            matches.push((id, 1));
            continue;
        }

        // Priority 2: Levenshtein distance <= 3
        let dist = levenshtein(&target_lower, &id_lower);
        if dist <= 3 && dist > 0 {
            matches.push((id, dist + 10)); // Offset to sort after prefix/suffix
        }
    }

    matches.sort_by_key(|(_, priority)| *priority);
    matches.into_iter().take(3).map(|(id, _)| id).collect()
}
