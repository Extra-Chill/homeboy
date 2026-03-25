//! pull — extracted from operations.rs.

use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use super::GitOutput;
use super::BulkIdsInput;


/// Pull remote changes for a component.
pub fn pull(component_id: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, None)?;
    let output =
        execute_git(&path, &["pull"]).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "pull", output))
}

/// Pull multiple components from JSON spec.
pub fn pull_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk pull input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    Ok(run_bulk_ids(&input.component_ids, "pull", |id| {
        pull(Some(id))
    }))
}
