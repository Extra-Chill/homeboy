use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use crate::json::{json_merge_patch, read_json_spec_to_string};
use crate::{Error, Result};

/// Trait for configuration types that support JSON input with merge-patch semantics.
///
/// Implementors get access to standardized create/update functionality via
/// `create_from_json()` which handles:
/// - Single or bulk object creation
/// - Upsert behavior (create if not exists, update if exists)
/// - RFC 7396 JSON Merge Patch for updates
pub trait ConfigImportable: DeserializeOwned + Serialize + Clone {
    /// The operation name for OpPayload validation (e.g., "project.create")
    fn op_name() -> &'static str;

    /// Type name for error messages (e.g., "project", "component")
    fn type_name() -> &'static str;

    /// Generate the unique ID for this config item
    fn config_id(&self) -> Result<String>;

    /// Check if config exists by ID
    fn exists(id: &str) -> bool;

    /// Load existing config by ID
    fn load(id: &str) -> Result<Self>;

    /// Save the config
    fn save(id: &str, config: &Self) -> Result<()>;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub id: String,
    pub action: CreateAction,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateAction {
    Created,
    Updated,
    Skipped,
    #[serde(rename = "error")]
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummary {
    pub results: Vec<CreateResult>,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Process JSON input for create command with upsert + merge-patch semantics.
///
/// Supports both single object and array of objects.
/// Uses RFC 7396 JSON Merge Patch when updating existing configs.
pub fn create_from_json<T: ConfigImportable>(
    json_spec: &str,
    skip_existing: bool,
) -> Result<CreateSummary> {
    let raw = read_json_spec_to_string(json_spec)?;

    // Parse as OpPayload with Value data to detect single vs array
    let payload: crate::json::OpPayload<Value> = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse op payload".to_string())))?;

    if payload.op != T::op_name() {
        return Err(Error::validation_invalid_argument(
            "op",
            format!("Unexpected op '{}'", payload.op),
            Some(T::op_name().to_string()),
            Some(vec![T::op_name().to_string()]),
        ));
    }

    // Normalize to array
    let items: Vec<Value> = match payload.data {
        Value::Array(arr) => arr,
        Value::Object(_) => vec![payload.data],
        _ => {
            return Err(Error::validation_invalid_argument(
                "data",
                "Expected object or array of objects",
                None,
                None,
            ))
        }
    };

    let mut results = Vec::with_capacity(items.len());
    let mut created = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for item in items {
        let result = process_single_item::<T>(item, skip_existing);
        match &result.action {
            CreateAction::Created => created += 1,
            CreateAction::Updated => updated += 1,
            CreateAction::Skipped => skipped += 1,
            CreateAction::Error { .. } => errors += 1,
        }
        results.push(result);
    }

    Ok(CreateSummary {
        results,
        created,
        updated,
        skipped,
        errors,
    })
}

fn process_single_item<T: ConfigImportable>(item: Value, skip_existing: bool) -> CreateResult {
    // First, try to extract the ID from the incoming JSON
    // We need to partially parse to get the name/id for lookup
    let id = match extract_id_from_value::<T>(&item) {
        Ok(id) => id,
        Err(e) => {
            return CreateResult {
                id: "unknown".to_string(),
                action: CreateAction::Error {
                    message: e.to_string(),
                },
            }
        }
    };

    // Check if exists
    if T::exists(&id) {
        if skip_existing {
            return CreateResult {
                id,
                action: CreateAction::Skipped,
            };
        }

        // Load existing and merge
        match merge_and_save::<T>(&id, item) {
            Ok(()) => CreateResult {
                id,
                action: CreateAction::Updated,
            },
            Err(e) => CreateResult {
                id,
                action: CreateAction::Error {
                    message: e.to_string(),
                },
            },
        }
    } else {
        // Create new
        match create_new::<T>(item) {
            Ok(new_id) => CreateResult {
                id: new_id,
                action: CreateAction::Created,
            },
            Err(e) => CreateResult {
                id,
                action: CreateAction::Error {
                    message: e.to_string(),
                },
            },
        }
    }
}

fn extract_id_from_value<T: ConfigImportable>(item: &Value) -> Result<String> {
    // Try to deserialize to get the config and extract ID
    let config: T = serde_json::from_value(item.clone())
        .map_err(|e| Error::validation_invalid_json(e, Some("parse config item".to_string())))?;
    config.config_id()
}

fn merge_and_save<T: ConfigImportable>(id: &str, incoming: Value) -> Result<()> {
    // Load existing config
    let existing = T::load(id)?;

    // Convert existing to Value for merging
    let mut existing_value = serde_json::to_value(&existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize existing".to_string())))?;

    // Apply merge patch
    json_merge_patch(&mut existing_value, incoming);

    // Deserialize merged back to T
    let merged: T = serde_json::from_value(existing_value)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse merged config".to_string())))?;

    // Verify ID didn't change (name might have been updated)
    let new_id = merged.config_id()?;
    if new_id != id {
        return Err(Error::validation_invalid_argument(
            "name",
            format!(
                "Cannot change {} name/id from '{}' to '{}' via merge",
                T::type_name(),
                id,
                new_id
            ),
            None,
            None,
        ));
    }

    // Save
    T::save(id, &merged)
}

fn create_new<T: ConfigImportable>(item: Value) -> Result<String> {
    let config: T = serde_json::from_value(item)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse new config".to_string())))?;
    let id = config.config_id()?;
    T::save(&id, &config)?;
    Ok(id)
}
