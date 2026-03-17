//! commit — extracted from operations.rs.

use crate::error::{Error, Result};
use super::tag;
use super::commit;
use super::super::changes::*;
use super::super::commits::*;
use super::super::*;


/// Get the commit SHA a tag points to.
pub fn get_tag_commit(path: &str, tag_name: &str) -> Result<String> {
    crate::engine::command::run_in(
        path,
        "git",
        &["rev-list", "-n", "1", tag_name],
        &format!("get commit for tag '{}'", tag_name),
    )
}

/// Get the current HEAD commit SHA.
pub fn get_head_commit(path: &str) -> Result<String> {
    crate::engine::command::run_in(path, "git", &["rev-parse", "HEAD"], "get HEAD commit")
}
