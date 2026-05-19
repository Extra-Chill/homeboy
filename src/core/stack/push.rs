//! `homeboy stack push` — publish a materialized target branch.
//!
//! `sync`/`apply` rebuild the local `target.branch`; `push` is the explicit
//! remote mutation step. Combined-fixes branches are rebuilt history, so this
//! uses `--force-with-lease` and never plain `--force`.

use serde::Serialize;

use crate::core::error::{Error, Result};

use super::git::run_git;
use super::spec::{resolve_existing_component_path, StackSpec};

/// Output envelope for `homeboy stack push`.
#[derive(Debug, Clone, Serialize)]
pub struct PushOutput {
    pub stack_id: String,
    pub component_path: String,
    pub remote: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_ref: Option<String>,
    pub after_ref: String,
    pub status: PushStatus,
    pub success: bool,
}

/// Whether the remote branch changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushStatus {
    Updated,
    Unchanged,
}

/// Push the stack's materialized local target branch to its configured remote.
pub fn push(spec: &StackSpec) -> Result<PushOutput> {
    let path = resolve_existing_component_path(spec)?;
    let remote = spec.target.remote.as_str();
    let branch = spec.target.branch.as_str();

    // Capture the remote ref first so the lease protects the exact state we
    // inspected, independent of local remote-tracking refs.
    let before = remote_branch_ref(&path, remote, branch)?;
    let local_ref = local_branch_ref(&path, branch)?;
    push_target_branch(&path, remote, branch, before.as_deref())?;
    let after = remote_branch_ref(&path, remote, branch)?.unwrap_or(local_ref);

    let status = if before.as_deref() == Some(after.as_str()) {
        PushStatus::Unchanged
    } else {
        PushStatus::Updated
    };

    Ok(PushOutput {
        stack_id: spec.id.clone(),
        component_path: path,
        remote: remote.to_string(),
        branch: branch.to_string(),
        before_ref: before,
        after_ref: after,
        status,
        success: true,
    })
}

pub(crate) fn remote_branch_ref(path: &str, remote: &str, branch: &str) -> Result<Option<String>> {
    let refspec = format!("refs/heads/{}", branch);
    let output = run_git(path, &["ls-remote", remote, &refspec])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git ls-remote {} {}: {}",
            remote,
            refspec,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .find_map(|line| line.split_whitespace().next().map(str::to_string)))
}

pub(crate) fn local_branch_ref(path: &str, branch: &str) -> Result<String> {
    let git_ref = format!("refs/heads/{}", branch);
    let output = run_git(path, &["rev-parse", &git_ref])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git rev-parse {}: {}",
            git_ref,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn push_target_branch(
    path: &str,
    remote: &str,
    branch: &str,
    expected_remote: Option<&str>,
) -> Result<()> {
    let source = format!("refs/heads/{}", branch);
    let destination = source.clone();
    let refspec = format!("{}:{}", source, destination);
    let lease = match expected_remote {
        Some(expected) => format!("--force-with-lease={}:{}", destination, expected),
        None => "--force-with-lease".to_string(),
    };
    let output = run_git(path, &["push", &lease, remote, &refspec])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git push --force-with-lease {} {}: {}",
            remote,
            refspec,
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/core/stack/push_test.rs"]
mod push_test;
