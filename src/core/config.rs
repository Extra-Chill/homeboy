use crate::error::Error;
use crate::json;
use crate::local_files::{self, FileSystem};
use crate::paths;
use crate::slugify;
use crate::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) trait ConfigEntity: Serialize + DeserializeOwned {
    fn id(&self) -> &str;
    fn set_id(&mut self, id: String);
    fn config_path(id: &str) -> Result<PathBuf>;
    fn config_dir() -> Result<PathBuf>;
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error;
    fn entity_type() -> &'static str;

    /// Entity-specific validation. Override to add custom validation rules.
    /// Called by `config::create()` before saving.
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
    let mut entity: T = json::from_str(&content)?;
    entity.set_id(id.to_string());
    Ok(entity)
}

pub(crate) fn list<T: ConfigEntity>() -> Result<Vec<T>> {
    let dir = T::config_dir()?;
    let entries = local_files::local().list(&dir)?;

    let mut items: Vec<T> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| {
            let id = e.path.file_stem()?.to_string_lossy().to_string();
            let content = local_files::local().read(&e.path).ok()?;
            let mut entity: T = json::from_str(&content).ok()?;
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
    let content = json::to_string_pretty(entity)?;
    local_files::local().write(&path, &content)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CreateResult<T> {
    pub id: String,
    pub entity: T,
}

/// Universal create function for any ConfigEntity.
/// Validates ID, checks for existence, runs entity-specific validation, then saves.
pub(crate) fn create<T: ConfigEntity>(entity: T) -> Result<CreateResult<T>> {
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
    let content = json::to_string_pretty(&entity)?;
    local_files::local().write(&path, &content)?;

    Ok(CreateResult {
        id: entity.id().to_string(),
        entity,
    })
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
// Batch Operations
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchResult {
    pub created: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    pub items: Vec<BatchResultItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchResultItem {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BatchResult {
    pub fn new() -> Self {
        Self {
            created: 0,
            updated: 0,
            skipped: 0,
            errors: 0,
            items: Vec::new(),
        }
    }

    pub fn record_created(&mut self, id: String) {
        self.created += 1;
        self.items.push(BatchResultItem {
            id,
            status: "created".to_string(),
            error: None,
        });
    }

    pub fn record_updated(&mut self, id: String) {
        self.updated += 1;
        self.items.push(BatchResultItem {
            id,
            status: "updated".to_string(),
            error: None,
        });
    }

    pub fn record_skipped(&mut self, id: String) {
        self.skipped += 1;
        self.items.push(BatchResultItem {
            id,
            status: "skipped".to_string(),
            error: None,
        });
    }

    pub fn record_error(&mut self, id: String, error: String) {
        self.errors += 1;
        self.items.push(BatchResultItem {
            id,
            status: "error".to_string(),
            error: Some(error),
        });
    }
}

impl Default for BatchResult {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Merge Operations
// ============================================================================

/// Unified output for merge operations (single or bulk).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MergeOutput {
    Single(json::MergeResult),
    Bulk(BatchResult),
}

/// Unified merge that auto-detects single vs bulk operations.
/// Array input triggers batch merge, object input triggers single merge.
pub fn merge<T: ConfigEntity>(id: Option<&str>, json_spec: &str) -> Result<MergeOutput> {
    let raw = json::read_json_spec_to_string(json_spec)?;

    if json::is_json_array(&raw) {
        return Ok(MergeOutput::Bulk(merge_batch_from_json::<T>(&raw)?));
    }

    Ok(MergeOutput::Single(merge_from_json::<T>(id, &raw)?))
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
    let value: serde_json::Value = json::from_str(spec)?;
    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().unwrap().clone()
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
) -> Result<json::MergeResult> {
    let raw = json::read_json_spec_to_string(json_spec)?;
    let mut parsed: serde_json::Value = json::from_str(&raw)?;

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
    let result = json::merge_config(&mut entity, parsed)?;
    entity.set_id(effective_id.clone());
    save(&entity)?;

    Ok(json::MergeResult {
        id: effective_id,
        updated_fields: result.updated_fields,
    })
}

pub(crate) fn merge_batch_from_json<T: ConfigEntity>(raw_json: &str) -> Result<BatchResult> {
    let value: serde_json::Value = json::from_str(raw_json)?;

    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().unwrap().clone()
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
            Ok(mut entity) => match json::merge_config(&mut entity, patch) {
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
) -> Result<json::RemoveResult> {
    let raw = json::read_json_spec_to_string(json_spec)?;
    let mut parsed: serde_json::Value = json::from_str(&raw)?;

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
    let result = json::remove_config(&mut entity, parsed)?;
    save(&entity)?;

    Ok(json::RemoveResult {
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
