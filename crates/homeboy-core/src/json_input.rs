use serde::{de::DeserializeOwned, Serialize};

/// Trait for commands that accept structured JSON input via --json flag.
/// Provides standardized bulk operation support with consistent error handling
/// and summary reporting.
pub trait JsonInput {
    /// The input item type (deserialized from JSON)
    type Item: DeserializeOwned;

    /// The result type for a single item
    type ItemResult: Serialize;

    /// Process a single item
    fn process_item(item: Self::Item) -> crate::Result<Self::ItemResult>;

    /// Check if an item succeeded (for summary calculation)
    fn is_success(result: &Self::ItemResult) -> bool;
}

/// Standardized bulk execution result
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkResult<T: Serialize> {
    pub action: String,
    pub results: Vec<ItemOutcome<T>>,
    pub summary: BulkSummary,
}

/// Outcome for a single item in a bulk operation
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemOutcome<T: Serialize> {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Summary of bulk operation results
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// Execute bulk operation using the JsonInput trait.
///
/// Takes a list of items and processes each one, collecting results and
/// generating a summary. The `id_extractor` function extracts an identifier
/// from each item for reporting purposes.
pub fn execute_bulk<T: JsonInput>(
    action: &str,
    items: Vec<T::Item>,
    id_extractor: impl Fn(&T::Item) -> String,
) -> (BulkResult<T::ItemResult>, i32) {
    let mut results = Vec::with_capacity(items.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for item in items {
        let id = id_extractor(&item);
        match T::process_item(item) {
            Ok(result) => {
                if T::is_success(&result) {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id,
                    result: Some(result),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id,
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let exit_code = if failed > 0 { 1 } else { 0 };

    (
        BulkResult {
            action: action.to_string(),
            results,
            summary: BulkSummary {
                total: succeeded + failed,
                succeeded,
                failed,
            },
        },
        exit_code,
    )
}

/// Simple bulk input with just component IDs
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdsInput {
    pub component_ids: Vec<String>,
}

/// Parse JSON spec into a BulkIdsInput
pub fn parse_bulk_ids(json_spec: &str) -> crate::Result<BulkIdsInput> {
    let raw = crate::json::read_json_spec_to_string(json_spec)?;
    serde_json::from_str(&raw)
        .map_err(|e| crate::Error::validation_invalid_json(e, Some("parse bulk IDs input".to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, serde::Deserialize)]
    struct TestItem {
        id: String,
        value: i32,
    }

    #[derive(Debug, Clone, Serialize)]
    struct TestResult {
        id: String,
        doubled: i32,
        success: bool,
    }

    struct TestProcessor;

    impl JsonInput for TestProcessor {
        type Item = TestItem;
        type ItemResult = TestResult;

        fn process_item(item: Self::Item) -> crate::Result<Self::ItemResult> {
            if item.value < 0 {
                return Err(crate::Error::other("Negative values not allowed".to_string()));
            }
            Ok(TestResult {
                id: item.id,
                doubled: item.value * 2,
                success: true,
            })
        }

        fn is_success(result: &Self::ItemResult) -> bool {
            result.success
        }
    }

    #[test]
    fn test_execute_bulk_all_success() {
        let items = vec![
            TestItem { id: "a".to_string(), value: 1 },
            TestItem { id: "b".to_string(), value: 2 },
            TestItem { id: "c".to_string(), value: 3 },
        ];

        let (result, exit_code) = execute_bulk::<TestProcessor>(
            "test",
            items,
            |item| item.id.clone(),
        );

        assert_eq!(exit_code, 0);
        assert_eq!(result.summary.total, 3);
        assert_eq!(result.summary.succeeded, 3);
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn test_execute_bulk_partial_failure() {
        let items = vec![
            TestItem { id: "a".to_string(), value: 1 },
            TestItem { id: "b".to_string(), value: -1 }, // Will fail
            TestItem { id: "c".to_string(), value: 3 },
        ];

        let (result, exit_code) = execute_bulk::<TestProcessor>(
            "test",
            items,
            |item| item.id.clone(),
        );

        assert_eq!(exit_code, 1);
        assert_eq!(result.summary.total, 3);
        assert_eq!(result.summary.succeeded, 2);
        assert_eq!(result.summary.failed, 1);
        assert!(result.results[1].error.is_some());
    }

    #[test]
    fn test_execute_bulk_empty() {
        let items: Vec<TestItem> = vec![];

        let (result, exit_code) = execute_bulk::<TestProcessor>(
            "test",
            items,
            |item| item.id.clone(),
        );

        assert_eq!(exit_code, 0);
        assert_eq!(result.summary.total, 0);
        assert_eq!(result.summary.succeeded, 0);
        assert_eq!(result.summary.failed, 0);
    }
}
