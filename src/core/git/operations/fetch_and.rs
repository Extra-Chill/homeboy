//! fetch_and — extracted from operations.rs.

use crate::error::{Error, Result};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::parse_ahead_behind;
use super::status;


/// Fetch from remote and return count of commits behind upstream.
/// Returns Ok(Some(n)) if behind by n commits, Ok(None) if not behind or no upstream.
pub fn fetch_and_get_behind_count(path: &str) -> Result<Option<u32>> {
    // Run git fetch (update tracking refs)
    crate::engine::command::run_in(path, "git", &["fetch"], "git fetch")?;

    // Check if upstream exists
    let upstream = crate::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
    );
    if upstream.is_none() {
        return Ok(None); // No upstream configured
    }

    // Get ahead/behind counts
    let counts = crate::engine::command::run_in_optional(
        path,
        "git",
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    );

    match counts {
        Some(output) => {
            let (_, behind) = parse_ahead_behind(&output);
            Ok(behind.filter(|&n| n > 0))
        }
        None => Ok(None),
    }
}

/// Fetch from remote and fast-forward if behind.
///
/// Returns Ok(Some(n)) with the number of commits fast-forwarded, or Ok(None) if
/// already up-to-date. Errors if the fast-forward fails (diverged histories).
pub fn fetch_and_fast_forward(path: &str) -> Result<Option<u32>> {
    let behind = fetch_and_get_behind_count(path)?;

    match behind {
        None => Ok(None),
        Some(n) => {
            // Attempt fast-forward pull
            let output = execute_git(path, &["pull", "--ff-only"])
                .map_err(|e| Error::git_command_failed(e.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::validation_invalid_argument(
                    "remote_sync",
                    format!(
                        "Branch has diverged from remote — fast-forward failed: {}",
                        stderr.trim()
                    ),
                    None,
                    Some(vec![
                        "Resolve the divergence manually before releasing".to_string(),
                        "Run: git pull --rebase".to_string(),
                    ]),
                ));
            }

            Ok(Some(n))
        }
    }
}
