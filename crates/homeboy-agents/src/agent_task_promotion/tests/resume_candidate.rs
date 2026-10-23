//! Real Git fixtures for candidate-only continuation admission.
use super::*;
use crate::agent_task::AgentTaskCandidate;
use crate::agent_task_promotion::AgentTaskPromotionRequest;
use serde_json::Value;
use std::path::Path;

pub(super) fn bind_candidate(
    mut options: AgentTaskPromotionRequest,
    root: &Path,
) -> AgentTaskPromotionRequest {
    let mut source: Value = serde_json::from_str(&options.source).unwrap();
    let outcome = if source.get("outcomes").is_some() {
        &mut source["outcomes"][0]
    } else {
        &mut source
    };
    let head = git_head(root, "HEAD");
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .unwrap();
    let base = if dirty.stdout.is_empty() {
        git_head(root, "HEAD^")
    } else {
        head.clone()
    };
    if !dirty.stdout.is_empty() {
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "candidate"]);
    }
    let candidate = AgentTaskCandidate {
        base_sha: base.clone(),
        candidate_sha: git_head(root, "HEAD"),
        tree_sha: git_head(root, "HEAD^{tree}"),
    };
    let run_id = options.source_run_id.as_deref().unwrap();
    git(
        root,
        &[
            "update-ref",
            &format!("refs/homeboy/candidates/{run_id}"),
            &candidate.candidate_sha,
        ],
    );
    outcome["candidate"] = serde_json::to_value(&candidate).unwrap();
    options.source = source.to_string();
    if head == base {
        git(root, &["reset", "--soft", &base]);
    }
    options
}

