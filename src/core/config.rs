mod config_entity_trait;
mod config_merge_remove;
mod generic_json_operations;
mod json_parsing_utilities;
mod json_pointer_operations;
mod optional_features;
mod types;
mod universal_wrappers_always;

pub use config_entity_trait::*;
pub use config_merge_remove::*;
pub use generic_json_operations::*;
pub use json_parsing_utilities::*;
pub use json_pointer_operations::*;
pub use optional_features::*;
pub use types::*;
pub use universal_wrappers_always::*;

use crate::engine::identifier;
use crate::engine::local_files::{self, FileSystem};
use crate::engine::text::levenshtein;
use crate::error::Error;
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

// ============================================================================
// JSON Pointer Operations (internal)
// ============================================================================

// ============================================================================
// Config Merge/Remove Operations (internal)
// ============================================================================

// ============================================================================
// Bulk Input Parsing
// ============================================================================

// ============================================================================
// Config Entity Trait
// ============================================================================

// ============================================================================
// Merge Operations
// ============================================================================

// ============================================================================
// Generic JSON Operations
// ============================================================================

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

        // --- Optional features ---
        $( $(entity_crud!(@feature $Entity, $feature);)+ )?
    };

    // Feature: list_ids
    (@feature $Entity:ty, list_ids) => {
    };

    // Feature: merge (standard one-liner)
    (@feature $Entity:ty, merge) => {
    };

    // Feature: slugify_id
    (@feature $Entity:ty, slugify_id) => {
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
