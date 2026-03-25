//! types — extracted from config.rs.

use crate::error::Error;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::paths;
use crate::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use crate::engine::local_files::{self, FileSystem};
use heck::ToSnakeCase;
use serde_json::{Map, Value};
use std::io::Read;


/// Internal result from merge_config (no ID, caller adds it).
#[derive(Debug)]
pub struct MergeFields {
    pub updated_fields: Vec<String>,
}

/// Internal result from remove_config (no ID, caller adds it).
pub struct RemoveFields {
    pub removed_from: Vec<String>,
}

/// Simple bulk input with just component IDs.
#[derive(Debug, Clone, Deserialize)]

pub struct BulkIdsInput {
    pub component_ids: Vec<String>,
}

pub trait ConfigEntity: Serialize + DeserializeOwned {
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
