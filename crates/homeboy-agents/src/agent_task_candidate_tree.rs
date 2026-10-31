use std::path::Path;
use std::process::Command;

use homeboy_core::{Error, Result};
use serde_json::Value;

/// Authenticate a continuation against its immutable candidate, never patch evidence.
pub(crate) fn validate_gate_feedback_candidate_baseline(
    root: &Path,
    baseline: &Value,
) -> Result<crate::agent_task::AgentTaskCandidate> {
    let candidate: crate::agent_task::AgentTaskCandidate = serde_json::from_value(
        baseline
            .get("candidate_commit")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|_| invalid("non-recoverable: baseline has no immutable candidate; run a new Cook"))?;
    crate::agent_task_promotion::validate_candidate_commit(root, &candidate)?;
    validate_immutable_candidate_tree(root, &candidate.candidate_sha)?;
    Ok(candidate)
}

pub(crate) fn validate_immutable_candidate_tree(root: &Path, candidate_sha: &str) -> Result<()> {
    if !matches!(candidate_sha.len(), 40 | 64)
        || !candidate_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("adopted candidate has no valid immutable commit"));
    }
    let expected_tree = git(root, &["rev-parse", &format!("{candidate_sha}^{{tree}}")])?;
    if workspace_tree(root)? != expected_tree {
        return Err(invalid(
            "candidate worktree differs from the adopted immutable commit",
        ));
    }
    Ok(())
}

pub(crate) fn workspace_tree(root: &Path) -> Result<String> {
    let index = tempfile::NamedTempFile::new().map_err(|error| invalid(&error.to_string()))?;
    let index_path = index.path().display().to_string();
    git_with_index(root, &["read-tree", "HEAD"], &index_path)?;
    git_with_index(root, &["add", "--all"], &index_path)?;
    git_with_index(root, &["write-tree"], &index_path)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    run_git(root, args, None)
}

fn git_with_index(root: &Path, args: &[&str], index_path: &str) -> Result<String> {
    run_git(root, args, Some(index_path))
}

fn run_git(root: &Path, args: &[&str], index: Option<&str>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
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
    use serde_json::json;

    fn candidate_repo() -> (tempfile::TempDir, Value) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-b", "main"]).unwrap();
        git(root, &["config", "user.name", "Homeboy Test"]).unwrap();
        git(root, &["config", "user.email", "test@example.test"]).unwrap();
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        git(root, &["add", "."]).unwrap();
        git(root, &["commit", "-m", "base"]).unwrap();
        let base = git(root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("tracked.txt"), "candidate\n").unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        git(root, &["add", "."]).unwrap();
        git(root, &["commit", "-m", "candidate"]).unwrap();
        let sha = git(root, &["rev-parse", "HEAD"]).unwrap();
        let tree = git(root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        (
            temp,
            json!({"candidate_commit":{"base_sha":base,"candidate_sha":sha,"tree_sha":tree}}),
        )
    }

    #[test]
    fn immutable_candidate_tree_accepts_exact_commit_and_rejects_dirty_state() {
        let (temp, baseline) = candidate_repo();
        let sha = baseline["candidate_commit"]["candidate_sha"]
            .as_str()
            .unwrap();
        validate_immutable_candidate_tree(temp.path(), sha).unwrap();
        std::fs::write(temp.path().join("tracked.txt"), "drift\n").unwrap();
        assert!(validate_immutable_candidate_tree(temp.path(), sha).is_err());
    }

    #[test]
    fn layered_remediation_uses_complete_candidate_commit() {
        let (temp, baseline) = candidate_repo();
        let candidate = validate_gate_feedback_candidate_baseline(temp.path(), &baseline).unwrap();
        assert_eq!(
            candidate.candidate_sha,
            git(temp.path(), &["rev-parse", "HEAD"]).unwrap()
        );
        std::fs::write(temp.path().join("unrelated.txt"), "drift\n").unwrap();
        assert!(validate_gate_feedback_candidate_baseline(temp.path(), &baseline).is_err());
    }

    #[test]
    fn patch_carried_baselines_are_non_recoverable() {
        let (temp, _) = candidate_repo();
        let error =
            validate_gate_feedback_candidate_baseline(temp.path(), &json!({"candidate":null}))
                .unwrap_err();
        assert!(error.message.contains("non-recoverable"));
    }
}
