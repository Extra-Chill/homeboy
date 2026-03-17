//! changes — extracted from operations.rs.

use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::project;
use super::ChangesOutput;
use super::build_bulk_changes_output;
use super::changes;
use super::BulkIdsInput;
use super::super::changes::*;
use super::super::commits::*;
use super::super::*;


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

/// Get changes for all components in a project.
pub fn changes_project(project_id: &str, include_diff: bool) -> Result<BulkResult<ChangesOutput>> {
    let proj = project::load(project_id)?;
    let component_ids: Vec<String> = project::resolve_project_components(&proj)?
        .into_iter()
        .map(|component| component.id)
        .collect();
    Ok(build_bulk_changes_output(&component_ids, include_diff))
}
