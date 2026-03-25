//! tag — extracted from operations.rs.

use crate::error::{Error, Result};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::GitOutput;


/// Create a git tag for a component.
pub fn tag(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
) -> Result<GitOutput> {
    tag_at(component_id, tag_name, message, None)
}

/// Like [`tag`] but with an explicit path override for git operations.
pub fn tag_at(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let name = tag_name.ok_or_else(|| {
        Error::validation_invalid_argument("tagName", "Missing tag name", None, None)
    })?;
    let (id, path) = resolve_target(component_id, path_override)?;
    let args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", name, "-m", msg],
        None => vec!["tag", name],
    };
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "tag", output))
}
