//! Public output types for Homeboy command responses.
//!
//! This extension contains all types that are part of the public API
//! for command output. These are used by CLI commands and consumers
//! of the homeboy library.

use serde::{Deserialize, Serialize};

// ============================================================================
// Create Operations
// ============================================================================

/// Result of a single create operation.
#[derive(Debug, Clone)]
pub struct CreateResult<T> {
    pub id: String,
    pub entity: T,
}

/// Unified output for create operations (single or bulk).
#[derive(Debug, Clone)]
pub enum CreateOutput<T> {
    Single(CreateResult<T>),
    Bulk(BatchResult),
}

// ============================================================================
// Merge Operations
// ============================================================================

/// Unified output for merge operations (single or bulk).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MergeOutput {
    Single(MergeResult),
    Bulk(BatchResult),
}

/// Result of a config merge operation.
#[derive(Debug, Clone, Serialize)]

pub struct MergeResult {
    pub id: String,
    pub updated_fields: Vec<String>,
}

/// Result of a config remove operation.
#[derive(Debug, Clone, Serialize)]

pub struct RemoveResult {
    pub id: String,
    pub removed_from: Vec<String>,
}

// ============================================================================
// Batch Operations
// ============================================================================

/// Summary of a batch create/update operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct BatchResult {
    pub created: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    pub items: Vec<BatchResultItem>,
}

/// Individual item result within a batch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct BatchResultItem {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BatchResult {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns 1 if any errors occurred, 0 otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.errors > 0 {
            1
        } else {
            0
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

// ============================================================================
// Bulk Operations (for commands that process multiple items)
// ============================================================================

/// Standardized bulk execution result.
#[derive(Debug, Serialize)]

pub struct BulkResult<T: Serialize> {
    pub action: String,
    pub results: Vec<ItemOutcome<T>>,
    pub summary: BulkSummary,
}

/// Outcome for a single item in a bulk operation.
#[derive(Debug, Serialize)]

pub struct ItemOutcome<T: Serialize> {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Summary of bulk operation results.
#[derive(Debug, Clone, Serialize)]

pub struct BulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

// ============================================================================
// Entity CRUD Output (generic for all entity commands)
// ============================================================================

/// Default extras type for entities with no extra fields.
#[derive(Debug, Default, Serialize)]
pub struct NoExtra;

/// Generic output for standard entity CRUD commands.
///
/// `T` is the entity type (Component, Server, Project, Fleet).
/// `E` is an optional extras struct for entity-specific fields, flattened
/// into the output JSON. Use `NoExtra` (the default) when no extras are needed.
///
/// Field naming is generic (`id`, `entity`, `entities`) rather than
/// entity-specific. Consumers use the `command` field to determine context.
#[derive(Debug, Serialize)]
pub struct EntityCrudOutput<T: Serialize, E: Serialize + Default = NoExtra> {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<T>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<T>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub updated_fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<BatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(flatten)]
    pub extra: E,
}

impl<T: Serialize, E: Serialize + Default> Default for EntityCrudOutput<T, E> {
    fn default() -> Self {
        Self {
            command: String::new(),
            id: None,
            entity: None,
            entities: Vec::new(),
            updated_fields: Vec::new(),
            deleted: Vec::new(),
            import: None,
            batch: None,
            hint: None,
            extra: E::default(),
        }
    }
}
