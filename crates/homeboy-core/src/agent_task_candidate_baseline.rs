use std::path::Path;
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Verify that a dirty worktree is exactly the gate-feedback candidate described
/// by its durable promotion artifact, without mutating its real Git index.
pub(crate) fn validate_gate_feedback_candidate_baseline(
    root: &Path,
    cook_loop: &Value,
) -> Result<String> {
    let artifact = cook_loop
        .get("patch_artifact")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("recorded patch artifact is not an object"))?;
    let path = artifact
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| invalid("recorded patch artifact has no path"))?;
    let expected_sha256 = artifact
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|sha256| !sha256.is_empty())
        .ok_or_else(|| invalid("recorded patch artifact has no sha256"))?;
    let patch = std::fs::read(path)
        .map_err(|error| invalid(&format!("recorded patch artifact is unreadable: {error}")))?;
    if format!("{:x}", Sha256::digest(&patch)) != expected_sha256 {
        return Err(invalid(
            "recorded patch artifact sha256 does not match its content",
        ));
    }
    let patch = String::from_utf8(patch)
        .map_err(|error| invalid(&format!("recorded patch artifact is not UTF-8: {error}")))?;
    if patch_tree(root, &patch)? != workspace_tree(root)? {
        return Err(invalid(
            "recorded patch artifact does not match the promoted candidate worktree state",
        ));
    }
    Ok(patch)
}

fn patch_tree(root: &Path, patch: &str) -> Result<String> {
    let index = tempfile::NamedTempFile::new().map_err(|error| invalid(&error.to_string()))?;
    let patch_file = tempfile::NamedTempFile::new().map_err(|error| invalid(&error.to_string()))?;
    std::fs::write(patch_file.path(), patch).map_err(|error| invalid(&error.to_string()))?;
    let index_path = index.path().display().to_string();
    git_with_index(root, &["read-tree", "HEAD"], &index_path)?;
    git_with_index(
        root,
        &[
            "apply",
            "--cached",
            "--binary",
            &patch_file.path().display().to_string(),
        ],
        &index_path,
    )?;
    git_with_index(root, &["write-tree"], &index_path)
}

fn workspace_tree(root: &Path) -> Result<String> {
    let index = tempfile::NamedTempFile::new().map_err(|error| invalid(&error.to_string()))?;
    let index_path = index.path().display().to_string();
    git_with_index(root, &["read-tree", "HEAD"], &index_path)?;
    git_with_index(root, &["add", "--all"], &index_path)?;
    git_with_index(root, &["write-tree"], &index_path)
}

fn git_with_index(root: &Path, args: &[&str], index_path: &str) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(root)
        .output()
        .map_err(|error| invalid(&error.to_string()))?;
    if !output.status.success() {
        return Err(invalid(&format!(
            "candidate baseline Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn invalid(message: &str) -> Error {
    Error::validation_invalid_argument(
        "gate_feedback_candidate_baseline",
        message.to_string(),
        None,
        None,
    )
}
