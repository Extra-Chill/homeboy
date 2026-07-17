use std::path::Path;
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::gate_feedback_baseline::{
    register_gate_feedback_baseline_provider, GateFeedbackBaselineProvider,
};
use crate::{Error, Result};

struct AgentTaskGateFeedbackBaselineProvider;

impl GateFeedbackBaselineProvider for AgentTaskGateFeedbackBaselineProvider {
    fn validate_gate_feedback_candidate_baseline(
        &self,
        root: &Path,
        baseline: &Value,
    ) -> Result<String> {
        validate_gate_feedback_candidate_baseline(root, baseline)
    }
}

/// Register the agent-task gate-feedback candidate-baseline provider. Called
/// once at startup so core's worktree-safety logic can accept a dirty worktree
/// that is a verified gate-feedback candidate without depending on the
/// agent-task subsystem.
pub fn register() {
    register_gate_feedback_baseline_provider(Box::new(AgentTaskGateFeedbackBaselineProvider));
}

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
    verify_patch_is_present(root, &patch)?;
    let current_diff = cook_loop
        .get("current_diff")
        .and_then(Value::as_str)
        .filter(|diff| !diff.trim().is_empty())
        .ok_or_else(|| invalid("gate-feedback candidate has no complete current diff"))?;
    let current_diff = format!("{}\n", current_diff.trim_end_matches('\n'));
    if patch_tree(root, &current_diff)? != workspace_tree(root)? {
        return Err(invalid(
            "recorded current diff does not match the promoted candidate worktree state",
        ));
    }
    Ok(current_diff)
}

fn verify_patch_is_present(root: &Path, patch: &str) -> Result<()> {
    let patch_file = tempfile::NamedTempFile::new().map_err(|error| invalid(&error.to_string()))?;
    std::fs::write(patch_file.path(), patch).map_err(|error| invalid(&error.to_string()))?;
    let output = Command::new("git")
        .args([
            "apply",
            "--reverse",
            "--check",
            "--binary",
            &patch_file.path().display().to_string(),
        ])
        .current_dir(root)
        .output()
        .map_err(|error| invalid(&error.to_string()))?;
    if !output.status.success() {
        return Err(invalid(&format!(
            "recorded patch artifact is not present in the candidate worktree: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_remediation_uses_complete_current_diff() {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), &["init", "-b", "main"]);
        git(temp.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            temp.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(temp.path().join("tracked.txt"), "one\n").expect("base file");
        git(temp.path(), &["add", "tracked.txt"]);
        git(temp.path(), &["commit", "-m", "base"]);

        std::fs::write(temp.path().join("tracked.txt"), "three\n").expect("candidate file");
        std::fs::write(temp.path().join("untracked.txt"), "candidate\n")
            .expect("untracked candidate file");
        git(temp.path(), &["add", "-N", "untracked.txt"]);
        let current_diff = git(temp.path(), &["diff", "--binary"]);
        let remediation = "diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-two\n+three\n";
        let artifacts = tempfile::tempdir().expect("artifact tempdir");
        let artifact = artifacts.path().join("remediation.patch");
        std::fs::write(&artifact, remediation).expect("remediation artifact");
        let transported_diff = current_diff.trim_end_matches('\n');
        let cook_loop = serde_json::json!({
            "current_diff": transported_diff,
            "patch_artifact": {
                "path": artifact,
                "sha256": format!("{:x}", Sha256::digest(remediation.as_bytes()))
            }
        });

        assert_eq!(
            validate_gate_feedback_candidate_baseline(temp.path(), &cook_loop)
                .expect("layered candidate baseline"),
            current_diff
        );
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}
