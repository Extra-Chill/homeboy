//! Fresh promotion must use Git objects, never patch evidence.
use super::super::promote::{
    promote_with_provider_and_checkpoint_in_observation_store, CandidateDestinationMoved,
    CandidateTreeMismatch,
};
use super::*;
use crate::agent_task::{AgentTaskCandidate, AgentTaskOutcome, AgentTaskOutcomeStatus};
use serde_json::json;

fn fixture(
    test: impl FnOnce(&Path, AgentTaskCandidate, &homeboy_core::observation::ObservationStore),
) {
    let _home = gate_home();
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "destination"]);
    git(&repo, &["config", "user.email", "test@example.test"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("base"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "base"]);
    let base_sha = git_head(&repo, "HEAD");
    // Provider commits remain in the ancestry of the harvested dirty state.
    std::fs::write(repo.join("provider"), "committed\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "provider"]);
    let provider_sha = git_head(&repo, "HEAD");
    std::fs::write(repo.join("dirty"), "uncommitted\n").unwrap();
    git(&repo, &["add", "."]);
    let tree_sha = String::from_utf8(
        Command::new("git")
            .arg("write-tree")
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let output = Command::new("git")
        .args([
            "commit-tree",
            &tree_sha,
            "-p",
            &provider_sha,
            "-m",
            "candidate",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    let candidate_sha = String::from_utf8(output.stdout).unwrap().trim().to_string();
    git(
        &repo,
        &["update-ref", "refs/homeboy/candidates/test", &candidate_sha],
    );
    git(&repo, &["reset", "--hard", &base_sha]);
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )
    .unwrap();
    test(
        &repo,
        AgentTaskCandidate {
            base_sha,
            candidate_sha,
            tree_sha,
        },
        &store,
    );
}

fn request(repo: &Path, candidate: Option<AgentTaskCandidate>) -> AgentTaskPromotionRequest {
    let mut options = promotion_options(repo.to_str().unwrap());
    options.source = serde_json::to_string(&AgentTaskOutcome {
        task_id: "test".into(),
        status: AgentTaskOutcomeStatus::Succeeded,
        candidate,
        artifacts: Vec::new(),
        ..Default::default()
    })
    .unwrap();
    options.source_worktree_path = Some(repo.into());
    options.gates.verify = vec!["true".to_string()];
    options
}

#[test]
fn fresh_candidate_without_patch_promotes_provider_commits_and_dirty_state_as_one() {
    fixture(|repo, candidate, store| {
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let branch = git_head(repo, "HEAD");
        let mut checkpoint = None;
        let report = promote_with_provider_and_checkpoint_in_observation_store(
            request(repo, Some(candidate.clone())),
            &mut provider,
            &mut |report| {
                checkpoint = Some(report.clone());
                Ok(())
            },
            store,
        )
        .unwrap();
        assert_eq!(
            report.status,
            super::super::AgentTaskPromotionStatus::Applied
        );
        assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
        assert_eq!(
            report.patch_artifact.sha256.as_deref(),
            Some(candidate.tree_sha.as_str())
        );
        assert_eq!(report.changed_files, vec!["dirty", "provider"]);
        let diff = Command::new("git")
            .args([
                "diff",
                "--name-only",
                &candidate.base_sha,
                &candidate.candidate_sha,
            ])
            .current_dir(repo)
            .output()
            .unwrap();
        assert_eq!(
            report.changed_files.join("\n"),
            String::from_utf8(diff.stdout).unwrap().trim()
        );
        assert_eq!(git_head(repo, "HEAD"), branch);
        assert_eq!(git_head(repo, "HEAD"), candidate.base_sha);
        let branch = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            "destination"
        );
        assert_eq!(
            report.provenance["safety_baseline"]["tree"],
            git_head(repo, "HEAD^{tree}")
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("dirty")).unwrap(),
            "uncommitted\n"
        );
        assert!(provider.apply_calls.is_empty());
        assert!(checkpoint.is_some());
        assert_eq!(
            report.provenance["candidate_checkout"]["commit"],
            candidate.candidate_sha
        );
    });
}

/// A gate-fix candidate builds on the earlier candidate, which the destination
/// still holds uncommitted on the original base (#16453).
#[test]
fn gate_fix_candidate_promotes_over_the_earlier_uncommitted_candidate() {
    fixture(|repo, first, store| {
        // The earlier promotion left its tree in the destination, with
        // `dirty` untracked rather than staged.
        git(
            repo,
            &["read-tree", "-u", "-m", "HEAD", &first.candidate_sha],
        );
        git(repo, &["rm", "--cached", "--quiet", "dirty"]);
        // The gate fix edits `provider` and deletes `dirty`.
        let scratch = tempfile::tempdir().unwrap();
        let index = scratch.path().join("index");
        let with_index = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .env("GIT_INDEX_FILE", &index)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        with_index(&["read-tree", &first.candidate_sha]);
        with_index(&["rm", "--cached", "--quiet", "dirty"]);
        let blob = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(b"fixed\n")?;
                child.wait_with_output()
            })
            .unwrap();
        let blob = String::from_utf8(blob.stdout).unwrap().trim().to_string();
        with_index(&[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},provider"),
        ]);
        let tree_sha = with_index(&["write-tree"]);
        let candidate_sha = with_index(&[
            "commit-tree",
            &tree_sha,
            "-p",
            &first.candidate_sha,
            "-m",
            "gate fix",
        ]);
        let second = AgentTaskCandidate {
            base_sha: first.candidate_sha.clone(),
            candidate_sha,
            tree_sha,
        };
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let report = promote_with_provider_and_checkpoint_in_observation_store(
            request(repo, Some(second.clone())),
            &mut provider,
            &mut |_| Ok(()),
            store,
        )
        .unwrap();
        assert_eq!(
            report.status,
            super::super::AgentTaskPromotionStatus::Applied
        );
        assert_eq!(git_head(repo, "HEAD"), first.base_sha);
        assert_eq!(
            crate::agent_task_candidate_tree::workspace_tree(repo).unwrap(),
            second.tree_sha
        );
        assert!(!repo.join("dirty").exists());
        assert_eq!(report.changed_files, vec!["provider"]);
        assert_eq!(
            report.verified_base.as_ref().map(|base| base.sha.as_str()),
            Some(first.base_sha.as_str())
        );
    });
}

#[test]
fn moved_destination_is_a_typed_error_and_never_materializes() {
    fixture(|repo, candidate, store| {
        git(
            repo,
            &["commit", "--allow-empty", "-m", "destination moved"],
        );
        let head = git_head(repo, "HEAD");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let error = promote_with_provider_and_checkpoint_in_observation_store(
            request(repo, Some(candidate)),
            &mut provider,
            &mut |_| panic!("must not checkpoint"),
            store,
        )
        .unwrap_err();
        assert!(std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<CandidateDestinationMoved>()
            .is_some());
        assert_eq!(git_head(repo, "HEAD"), head);
        assert!(!repo.join("dirty").exists());
    });
}

#[test]
fn recorded_tree_mismatch_is_a_typed_error_before_destination_mutation() {
    fixture(|repo, mut candidate, store| {
        candidate.tree_sha = git_head(repo, "HEAD^{tree}");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let error = promote_with_provider_and_checkpoint_in_observation_store(
            request(repo, Some(candidate)),
            &mut provider,
            &mut |_| panic!("must not checkpoint"),
            store,
        )
        .unwrap_err();
        assert!(std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<CandidateTreeMismatch>()
            .is_some());
        assert!(!repo.join("dirty").exists());
    });
}

#[test]
fn no_candidate_and_no_change_never_runs_gates() {
    fixture(|repo, _, store| {
        let mut options = request(repo, None);
        let mut source: serde_json::Value = serde_json::from_str(&options.source).unwrap();
        source["status"] = json!("no_op");
        options.source = source.to_string();
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let report = promote_with_provider_and_checkpoint_in_observation_store(
            options,
            &mut provider,
            &mut |_| panic!("no change must not checkpoint"),
            store,
        )
        .unwrap();
        assert_eq!(
            report.status,
            super::super::AgentTaskPromotionStatus::NoChanges
        );
        assert!(report.changed_files.is_empty());
        assert!(provider.verify_calls.is_empty());
        assert!(provider.apply_calls.is_empty());
    });
}

#[test]
fn patch_only_records_are_explicitly_non_recoverable() {
    fixture(|repo, _, store| {
        let mut options = request(repo, None);
        let mut source: serde_json::Value = serde_json::from_str(&options.source).unwrap();
        source["artifacts"] = json!([{"schema": AGENT_TASK_ARTIFACT_SCHEMA, "id": "patch", "kind": "patch", "path": "missing.patch"}]);
        options.source = source.to_string();
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.into()),
            ..Default::default()
        };
        let error = promote_with_provider_and_checkpoint_in_observation_store(
            options,
            &mut provider,
            &mut |_| panic!("must not checkpoint"),
            store,
        )
        .unwrap_err();
        assert!(error.message.contains("non-recoverable"));
        assert_eq!(error.retryable, Some(false));
    });
}
