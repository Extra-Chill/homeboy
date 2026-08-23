use serde::Serialize;

use crate::error::Result;
use crate::output::{BulkResult, BulkResultBuilder};

const FAILURE_STREAM_LIMIT: usize = 4 * 1024;

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

    /// Convert a failed Git operation into the generic, safe command evidence
    /// carried by structured errors and durable recovery records.
    pub fn failure_command_evidence(&self) -> homeboy_error::CommandEvidence {
        let policy = crate::redaction::RedactionPolicy::default();
        let (stdout, stdout_truncated) = bounded_stream(&policy.redact_string(&self.stdout));
        let (stderr, stderr_truncated) = bounded_stream(&policy.redact_string(&self.stderr));
        homeboy_error::CommandEvidence {
            command: format!("git {}", self.action),
            // Local worktree paths are not operator-facing failure evidence.
            cwd: None,
            location: Some("local".to_string()),
            exit_code: self.exit_code,
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
        }
    }
}

fn bounded_stream(value: &str) -> (String, bool) {
    if value.len() <= FAILURE_STREAM_LIMIT {
        return (value.to_string(), false);
    }
    let mut end = FAILURE_STREAM_LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
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
    let mut builder = BulkResultBuilder::with_capacity(action, ids.len());

    for id in ids {
        match op(id) {
            Ok(output) => {
                if output.success {
                    builder.record_success(id.clone(), output);
                } else {
                    builder.record_failed_result(id.clone(), output);
                }
            }
            Err(e) => {
                builder.record_error(id.clone(), e.to_string());
            }
        }
    }

    builder.finish()
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

    #[test]
    fn add_failure_evidence_keeps_stdout_and_does_not_expose_the_worktree_path() {
        let output = GitOutput {
            component_id: "local".to_string(),
            path: "/private/worktree".to_string(),
            action: "add".to_string(),
            success: false,
            exit_code: 17,
            stdout: "x".repeat(FAILURE_STREAM_LIMIT + 1),
            stderr: String::new(),
        };

        let evidence = output.failure_command_evidence();
        assert_eq!(evidence.command, "git add");
        assert_eq!(evidence.exit_code, 17);
        assert_eq!(evidence.stdout.len(), FAILURE_STREAM_LIMIT);
        assert!(evidence.truncated);
        assert!(evidence.cwd.is_none());
    }

    #[test]
    fn commit_failure_evidence_keeps_stderr() {
        let output = GitOutput {
            component_id: "local".to_string(),
            path: "/private/worktree".to_string(),
            action: "commit".to_string(),
            success: false,
            exit_code: 1,
            stdout: String::new(),
            stderr: "author identity unknown".to_string(),
        };

        let evidence = output.failure_command_evidence();
        assert_eq!(evidence.command, "git commit");
        assert_eq!(evidence.stderr, "author identity unknown");
        assert!(evidence.stdout.is_empty());
    }

    #[test]
    fn failure_evidence_uses_the_shared_redaction_policy() {
        let output = GitOutput {
            component_id: "local".to_string(),
            path: "/private/worktree".to_string(),
            action: "commit".to_string(),
            success: false,
            exit_code: 1,
            stdout: "token=super-secret-value".to_string(),
            stderr: String::new(),
        };

        let evidence = output.failure_command_evidence();
        assert!(!evidence.stdout.contains("super-secret-value"));
    }
}
