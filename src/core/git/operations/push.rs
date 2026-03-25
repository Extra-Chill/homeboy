//! push — extracted from operations.rs.

use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use super::run_bulk_ids;
use super::BulkIdsInput;
use super::GitOutput;


/// Push local commits for a component.
pub fn push(component_id: Option<&str>, tags: bool) -> Result<GitOutput> {
    push_at(component_id, tags, None)
}

/// Like [`push`] but with an explicit path override for git operations.
pub fn push_at(
    component_id: Option<&str>,
    tags: bool,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let args: Vec<&str> = if tags {
        vec!["push", "--follow-tags"]
    } else {
        vec!["push"]
    };
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "push", output))
}

/// Push multiple components from JSON spec.
pub fn push_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk push input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    let push_tags = input.tags;
    Ok(run_bulk_ids(&input.component_ids, "push", |id| {
        push(Some(id), push_tags)
    }))
}
