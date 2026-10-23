//! Migration fixture: build immutable commits for tests of gates/checkpoints.
//! Patch admission tests use the retained resume/recover admission helpers.
use super::super::promote;
use super::*;
use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
use serde_json::{json, Value};

fn candidate_options(
    mut options: AgentTaskPromotionRequest,
    provider: &FakePromotionWorkspaceProvider,
    store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionRequest> {
    if options.candidate_ref.is_some() {
        return Ok(options);
    }
    let mut source: Value = serde_json::from_str(&options.source).unwrap();
    let outcome_value = if source.get("outcomes").is_some() {
        &mut source["outcomes"][0]
    } else {
        &mut source
    };
    let outcome: AgentTaskOutcome = serde_json::from_value(outcome_value.clone()).unwrap();
    if outcome.candidate.is_some() {
        return Ok(options);
    }
    let patch = if outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable
        && (outcome.artifacts.is_empty()
            || !outcome.artifacts.iter().all(|a| a.size_bytes == Some(0)))
    {
        let admission = promote::recoverable_candidate_promotion_admission(
            &serde_json::from_str(&options.source).unwrap(),
            &outcome,
            &options,
            store,
        )?;
        Some(promote::patch_artifact_admission(
            &admission.artifact,
            &outcome,
            &options,
            store,
        )?)
    } else if !outcome.artifacts.is_empty() {
        let artifact = promote::select_patch_artifact(&outcome, options.artifact_id.as_deref())?;
        Some(promote::patch_artifact_admission(
            &artifact, &outcome, &options, store,
        )?)
    } else {
        None
    };
    let path = provider
        .workspace_path
        .as_ref()
        .or(options.source_worktree_path.as_ref())
        .unwrap();
    if !Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(path)
        .output()
        .is_ok_and(|output| output.status.success())
    {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "fixture@example.test"]);
        git(path, &["config", "user.name", "Fixture"]);
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(path.join("src/lib.rs"), "old\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "base"]);
    }
    let base = options
        .task_base_sha
        .clone()
        .unwrap_or_else(|| git_head(path, "HEAD"));
    if let Some(admission) = patch.as_ref().filter(|p| !p.patch.trim().is_empty()) {
        let normalized = admission.normalized_patch.as_ref().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &normalized.content).unwrap();
        git(path, &["apply", file.path().to_str().unwrap()]);
    }
    if provider.force_add_ignored_file {
        std::fs::write(path.join(".git/info/exclude"), "ignored/\n").unwrap();
        std::fs::create_dir_all(path.join("ignored/nested")).unwrap();
        std::fs::write(
            path.join("ignored/nested/force-added.rs"),
            "pub const FORCED: bool = true;\n",
        )
        .unwrap();
        git(path, &["add", "-f", "ignored/nested/force-added.rs"]);
    }
    git(path, &["add", "-A"]);
    let tree = git_head(path, "HEAD^{tree}");
    let staged = Command::new("git")
        .args(["write-tree"])
        .current_dir(path)
        .output()
        .unwrap();
    let staged = String::from_utf8(staged.stdout).unwrap().trim().to_string();
    if tree != staged {
        git(path, &["commit", "-m", "fixture candidate"]);
    }
    let candidate = git_head(path, "HEAD");
    let candidate_tree = git_head(path, "HEAD^{tree}");
    let base_tree = git_head(path, &format!("{base}^{{tree}}"));
    if candidate_tree == base_tree {
        outcome_value["status"] = json!("no_op");
    } else {
        outcome_value["candidate"] =
            json!({"base_sha": base, "candidate_sha": candidate, "tree_sha": candidate_tree});
        git(path, &["reset", "--hard", &base]);
    }
    options.source = source.to_string();
    Ok(options)
}

pub(super) fn promote_with_provider(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
) -> Result<AgentTaskPromotionReport> {
    promote_with_provider_and_checkpoint(options, provider, &mut |_| Ok(()))
}
pub(super) fn promote_with_provider_and_checkpoint(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )?;
    promote_with_provider_and_checkpoint_in_observation_store(options, provider, checkpoint, &store)
}
pub(super) fn promote_with_provider_in_observation_store(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
    store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionReport> {
    promote_with_provider_and_checkpoint_in_observation_store(
        options,
        provider,
        &mut |_| Ok(()),
        store,
    )
}
pub(super) fn promote_with_provider_and_checkpoint_in_observation_store(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
    store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionReport> {
    let options = candidate_options(options, provider, store)?;
    promote::promote_with_provider_and_checkpoint_in_observation_store(
        options, provider, checkpoint, store,
    )
}
