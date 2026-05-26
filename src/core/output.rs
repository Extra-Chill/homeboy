//! Public output types for Homeboy command responses.
//!
//! This extension contains all types that are part of the public API
//! for command output. These are used by CLI commands and consumers
//! of the homeboy library.

use serde::{Deserialize, Serialize};

// ============================================================================
// Observation-backed Outputs
// ============================================================================

/// Compact pointer from a command result to its persisted observation record.
///
/// Command outputs keep their existing fields for compatibility. Observation-
/// backed commands can add this metadata when the best-effort observation store
/// is available, giving wrappers a stable run ID and exact drill-down commands
/// without forcing every `--output` artifact to duplicate the full evidence set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationOutputMetadata {
    pub schema: String,
    pub run_id: String,
    pub kind: String,
    pub details: ObservationOutputDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationOutputDetails {
    pub query: String,
    pub artifacts: String,
    pub export_bundle: String,
}

impl ObservationOutputMetadata {
    pub fn for_run(kind: impl Into<String>, run_id: impl Into<String>) -> Self {
        let kind = kind.into();
        let run_id = run_id.into();
        Self {
            schema: "homeboy/observation-pointer/v1".to_string(),
            run_id: run_id.clone(),
            kind,
            details: ObservationOutputDetails {
                query: format!("homeboy runs show {run_id}"),
                artifacts: format!("homeboy runs artifacts {run_id}"),
                export_bundle: format!(
                    "homeboy runs export --run {run_id} --output ~/.local/share/homeboy/exports/{run_id}"
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_for_run() {
        let metadata = ObservationOutputMetadata::for_run("review", "run-123");
        let json = serde_json::to_value(metadata).expect("serialize observation metadata");

        assert_eq!(json["schema"], "homeboy/observation-pointer/v1");
        assert_eq!(json["run_id"], "run-123");
        assert_eq!(json["kind"], "review");
        assert_eq!(json["details"]["query"], "homeboy runs show run-123");
        assert_eq!(
            json["details"]["artifacts"],
            "homeboy runs artifacts run-123"
        );
        assert_eq!(
            json["details"]["export_bundle"],
            "homeboy runs export --run run-123 --output ~/.local/share/homeboy/exports/run-123"
        );
    }

    #[test]
    fn test_exit_code() {
        let clean = BatchResult::new();
        assert_eq!(clean.exit_code(), 0);

        let mut failed = BatchResult::new();
        failed.record_error("item".to_string(), "failed".to_string());
        assert_eq!(failed.exit_code(), 1);
    }

    #[test]
    fn test_record_created() {
        let mut result = BatchResult::new();
        result.record_created("alpha".to_string());

        assert_eq!(result.created, 1);
        assert_eq!(result.items[0].id, "alpha");
        assert_eq!(result.items[0].status, "created");
        assert_eq!(result.items[0].error, None);
    }

    #[test]
    fn test_record_updated() {
        let mut result = BatchResult::new();
        result.record_updated("alpha".to_string());

        assert_eq!(result.updated, 1);
        assert_eq!(result.items[0].id, "alpha");
        assert_eq!(result.items[0].status, "updated");
        assert_eq!(result.items[0].error, None);
    }

    #[test]
    fn test_record_skipped() {
        let mut result = BatchResult::new();
        result.record_skipped("alpha".to_string());

        assert_eq!(result.skipped, 1);
        assert_eq!(result.items[0].id, "alpha");
        assert_eq!(result.items[0].status, "skipped");
        assert_eq!(result.items[0].error, None);
    }

    #[test]
    fn test_record_error() {
        let mut result = BatchResult::new();
        result.record_error("alpha".to_string(), "boom".to_string());

        assert_eq!(result.errors, 1);
        assert_eq!(result.items[0].id, "alpha");
        assert_eq!(result.items[0].status, "error");
        assert_eq!(result.items[0].error.as_deref(), Some("boom"));
    }

    #[test]
    fn batch_outcome_totals_cover_success_partial_failure_skipped_and_empty() {
        let empty = BatchResult::new();
        assert_eq!(empty.outcome_totals(), OutcomeTotals::default());

        let mut result = BatchResult::new();
        result.record_created("created".to_string());
        result.record_updated("updated".to_string());
        result.record_skipped("skipped".to_string());
        result.record_error("failed".to_string(), "boom".to_string());

        assert_eq!(
            result.outcome_totals(),
            OutcomeTotals {
                total: 4,
                succeeded: 2,
                failed: 1,
                skipped: 1,
            }
        );
        assert_eq!(result.exit_code(), 1);
    }

    #[test]
    fn bulk_result_builds_summary_without_changing_json_shape() {
        let output = BulkResult::new(
            "fixture",
            vec![
                ItemOutcome::success("alpha", json!({ "value": 1 })),
                ItemOutcome::error("beta", "boom"),
            ],
        );

        let serialized = serde_json::to_value(output).expect("serialize bulk result");
        assert_eq!(serialized["action"], "fixture");
        assert_eq!(serialized["summary"]["total"], 2);
        assert_eq!(serialized["summary"]["succeeded"], 1);
        assert_eq!(serialized["summary"]["failed"], 1);
        assert!(serialized["summary"].get("skipped").is_none());
        assert_eq!(serialized["results"][0]["id"], "alpha");
        assert_eq!(serialized["results"][0]["value"], 1);
        assert_eq!(serialized["results"][1]["id"], "beta");
        assert_eq!(serialized["results"][1]["error"], "boom");
    }

    #[test]
    fn bulk_builder_counts_failed_results_without_changing_item_shape() {
        let mut builder = BulkResultBuilder::new("fixture");
        builder.record_success("alpha", json!({ "success": true }));
        builder.record_failed_result("beta", json!({ "success": false }));

        let serialized = serde_json::to_value(builder.finish()).expect("serialize bulk result");
        assert_eq!(serialized["summary"]["total"], 2);
        assert_eq!(serialized["summary"]["succeeded"], 1);
        assert_eq!(serialized["summary"]["failed"], 1);
        assert_eq!(serialized["results"][1]["id"], "beta");
        assert_eq!(serialized["results"][1]["success"], false);
        assert!(serialized["results"][1].get("error").is_none());
    }

    #[test]
    fn bulk_result_handles_empty_results() {
        let output = BulkResult::<serde_json::Value>::new("fixture", Vec::new());

        assert_eq!(output.summary.total, 0);
        assert_eq!(output.summary.succeeded, 0);
        assert_eq!(output.summary.failed, 0);
        assert!(output.results.is_empty());
    }
}

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

/// Shared outcome counters used by batch and bulk result adapters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutcomeTotals {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
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

    pub fn outcome_totals(&self) -> OutcomeTotals {
        OutcomeTotals {
            total: self.items.len(),
            succeeded: (self.created + self.updated) as usize,
            failed: self.errors as usize,
            skipped: self.skipped as usize,
        }
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

impl<T: Serialize> BulkResult<T> {
    pub fn new(action: impl Into<String>, results: Vec<ItemOutcome<T>>) -> Self {
        let summary = BulkSummary::from_outcome_totals(OutcomeTotals {
            total: results.len(),
            succeeded: results.iter().filter(|result| result.succeeded()).count(),
            failed: results.iter().filter(|result| result.failed()).count(),
            skipped: 0,
        });

        Self {
            action: action.into(),
            results,
            summary,
        }
    }
}

pub struct BulkResultBuilder<T: Serialize> {
    action: String,
    results: Vec<ItemOutcome<T>>,
    totals: OutcomeTotals,
}

impl<T: Serialize> BulkResultBuilder<T> {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            results: Vec::new(),
            totals: OutcomeTotals::default(),
        }
    }

    pub fn with_capacity(action: impl Into<String>, capacity: usize) -> Self {
        Self {
            action: action.into(),
            results: Vec::with_capacity(capacity),
            totals: OutcomeTotals::default(),
        }
    }

    pub fn record_success(&mut self, id: impl Into<String>, result: T) {
        self.totals.total += 1;
        self.totals.succeeded += 1;
        self.results.push(ItemOutcome::success(id, result));
    }

    pub fn record_failed_result(&mut self, id: impl Into<String>, result: T) {
        self.totals.total += 1;
        self.totals.failed += 1;
        self.results.push(ItemOutcome::success(id, result));
    }

    pub fn record_error(&mut self, id: impl Into<String>, error: impl Into<String>) {
        self.totals.total += 1;
        self.totals.failed += 1;
        self.results.push(ItemOutcome::error(id, error));
    }

    pub fn finish(self) -> BulkResult<T> {
        BulkResult {
            action: self.action,
            results: self.results,
            summary: BulkSummary::from_outcome_totals(self.totals),
        }
    }
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

impl<T: Serialize> ItemOutcome<T> {
    pub fn success(id: impl Into<String>, result: T) -> Self {
        Self {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            result: None,
            error: Some(error.into()),
        }
    }

    fn succeeded(&self) -> bool {
        self.result.is_some() && self.error.is_none()
    }

    fn failed(&self) -> bool {
        !self.succeeded()
    }
}

/// Summary of bulk operation results.
#[derive(Debug, Clone, Serialize)]

pub struct BulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

impl BulkSummary {
    pub fn from_outcome_totals(totals: OutcomeTotals) -> Self {
        Self {
            total: totals.total,
            succeeded: totals.succeeded,
            failed: totals.failed,
        }
    }
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
