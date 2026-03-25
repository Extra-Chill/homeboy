//! bulk — extracted from operations.rs.

use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use serde::{Deserialize, Serialize};
use std::process::Command;
use super::BulkIdsInput;
use super::push;
use super::ChangesOutput;
use super::GitOutput;


pub(crate) fn run_bulk_ids<F>(ids: &[String], action: &str, op: F) -> BulkResult<GitOutput>
where
    F: Fn(&str) -> Result<GitOutput>,
{
    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in ids {
        match op(id) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    BulkResult {
        action: action.to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    }
}

/// Get changes for multiple components from JSON spec.
pub fn changes_bulk(json_spec: &str, include_diff: bool) -> Result<BulkResult<ChangesOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk changes input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(build_bulk_changes_output(
        &input.component_ids,
        include_diff,
    ))
}
