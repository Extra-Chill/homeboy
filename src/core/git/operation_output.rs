use serde::Serialize;

use crate::core::error::Result;
use crate::core::output::{BulkResult, BulkSummary, ItemOutcome};

#[derive(Debug, Clone, Serialize)]
pub struct GitOutput {
    pub component_id: String,
    pub path: String,
    pub action: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub(crate) fn from_output(
        id: String,
        path: String,
        action: &str,
        output: std::process::Output,
    ) -> Self {
        Self {
            component_id: id,
            path,
            action: action.to_string(),
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(1),
            stdout: scrub_git_secrets(&String::from_utf8_lossy(&output.stdout)),
            stderr: scrub_git_secrets(&String::from_utf8_lossy(&output.stderr)),
        }
    }
}

fn scrub_git_secrets(value: &str) -> String {
    let mut scrubbed = String::with_capacity(value.len());
    let mut rest = value;
    const NEEDLE: &str = "x-access-token:";

    while let Some(start) = rest.find(NEEDLE) {
        let token_start = start + NEEDLE.len();
        scrubbed.push_str(&rest[..token_start]);
        if let Some(end) = rest[token_start..].find('@') {
            scrubbed.push_str("[REDACTED]");
            scrubbed.push('@');
            rest = &rest[token_start + end + 1..];
        } else {
            scrubbed.push_str("[REDACTED]");
            rest = "";
        }
    }

    scrubbed.push_str(rest);
    scrubbed
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_git_secrets_redacts_x_access_token_urls() {
        let output = scrub_git_secrets(
            "fatal: could not read https://x-access-token:ghs_secret123@github.com/owner/repo.git",
        );

        assert!(!output.contains("ghs_secret123"));
        assert!(output.contains("https://x-access-token:[REDACTED]@github.com/owner/repo.git"));
    }
}
