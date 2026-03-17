//! tag_exists — extracted from operations.rs.

use crate::error::{Error, Result};
use super::tag;
use super::super::changes::*;
use super::super::commits::*;
use super::super::*;


/// Check if a tag exists on the remote.
pub fn tag_exists_on_remote(path: &str, tag_name: &str) -> Result<bool> {
    Ok(crate::engine::command::run_in_optional(
        path,
        "git",
        &[
            "ls-remote",
            "--tags",
            "origin",
            &format!("refs/tags/{}", tag_name),
        ],
    )
    .map(|s| !s.is_empty())
    .unwrap_or(false))
}

/// Check if a tag exists locally.
pub fn tag_exists_locally(path: &str, tag_name: &str) -> Result<bool> {
    Ok(
        crate::engine::command::run_in_optional(path, "git", &["tag", "-l", tag_name])
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    )
}
