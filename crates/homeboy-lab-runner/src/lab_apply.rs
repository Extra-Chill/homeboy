use std::path::{Path, PathBuf};

use homeboy_agents::agent_task::AgentTaskCandidate;
use homeboy_agents::agent_task_lifecycle::canonical_runner_artifact_ref;
use homeboy_core::change_artifact::{
    ChangeApplyResult, ChangeArtifact, ChangePatch, CHANGE_ARTIFACT_SCHEMA,
    UNIFIED_DIFF_PATCH_FORMAT,
};
use homeboy_core::paths::sanitize_path_segment;
use homeboy_core::{Error, Result};
use serde::Deserialize;
use serde_json::Value;

use homeboy_core::runner_download_cache::RunnerDownloadIntent;

use super::{
    apply_change_artifact, download_remote_artifact_with_intent, is_retrievable_runner_artifact,
    RunnerExecOutput, RunnerWorkspaceApplyOutput,
};
use crate::workspace::git_output;

pub(super) fn apply_lab_offload_patch(
    exec_output: &RunnerExecOutput,
) -> Result<Option<RunnerWorkspaceApplyOutput>> {
    let Some(patch) = exec_output.patch.as_ref() else {
        return Ok(None);
    };
    let modified_files = patch
        .get("modified_files")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(patch_path) = patch
        .get("patch_artifact_path")
        .and_then(|value| value.as_str())
    else {
        if modified_files.is_empty() {
            return Ok(None);
        }
        return Err(Error::internal_unexpected(
            "Runner execution captured modified files but did not return a patch artifact path",
        ));
    };
    let source_snapshot = exec_output.source_snapshot.clone().ok_or_else(|| {
        Error::internal_unexpected("Runner patch apply requires the source snapshot")
    })?;
    let patch_content = read_lab_patch_artifact(patch_path)?;
    if patch_content.trim().is_empty() {
        return Ok(None);
    }

    let artifact = ChangeArtifact {
        schema: CHANGE_ARTIFACT_SCHEMA.to_string(),
        source_snapshot,
        patch: Some(ChangePatch {
            format: UNIFIED_DIFF_PATCH_FORMAT.to_string(),
            content: patch_content,
        }),
        delta: None,
        provenance: None,
        digest: None,
    };

    apply_change_artifact(artifact, false).map(Some)
}

/// A Lab promotion's candidate commit and the runner artifact bundling it (#16303).
#[derive(Debug, Deserialize)]
pub(super) struct LabCandidateHandback {
    run_id: String,
    task_id: String,
    artifact_id: String,
    candidate: AgentTaskCandidate,
}

/// Why a Lab candidate was refused, naming (expected, observed). Never falls back to a patch.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CandidateBundleError {
    /// The bundle is truncated or corrupt, or lacks the candidate: (commit, git error).
    Unreadable(String, String),
    CommitMismatch(String, String),
    TreeMismatch(String, String),
    /// The destination has uncommitted changes: (`git status`).
    DirtyDestination(String),
    /// Destination HEAD does not precede the candidate: (base, HEAD).
    UnrelatedDestination(String, String),
}

impl From<CandidateBundleError> for Error {
    fn from(error: CandidateBundleError) -> Self {
        let message = format!("Lab candidate refused: {error:?}");
        Error::validation_invalid_argument("promotion_handoff.candidate", message, None, None)
    }
}

/// An applied or gate-failed Lab promotion must hand back its candidate commit.
pub(super) fn promotion_candidate_handback(
    args: &[String],
    stdout: &str,
) -> Result<Option<LabCandidateHandback>> {
    if !args
        .windows(2)
        .any(|pair| pair == ["agent-task", "promote"])
    {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(stdout).map_err(|error| {
        let context = Some("portable promotion result".to_string());
        Error::validation_invalid_json(error, context, Some(stdout.to_string()))
    })?;
    let report = value.get("data").unwrap_or(&value);
    if !matches!(report["status"].as_str(), Some("applied" | "gate_failed")) {
        return Ok(None);
    }
    let handback = report.pointer("/provenance/lab_offload/candidate_handback");
    let handback = handback.and_then(|value| serde_json::from_value(value.clone()).ok());
    handback.map(Some).ok_or_else(|| {
        let message = "Lab promotion result hands back no candidate commit; patch handback results are not recoverable (#16303)";
        Error::validation_invalid_argument("promotion_handoff.candidate", message, None, None)
    })
}

/// Fetch the runner's candidate bundle into the controller destination and
/// make that worktree exactly the candidate tree.
pub(super) fn apply_lab_candidate(
    runner_id: &str,
    exec_output: &RunnerExecOutput,
    handback: &LabCandidateHandback,
) -> Result<RunnerWorkspaceApplyOutput> {
    let snapshot = exec_output.source_snapshot.as_ref().ok_or_else(|| {
        Error::internal_unexpected("Lab candidate handback requires the source snapshot")
    })?;
    let destination = crate::apply::local_source_path(snapshot)?;
    let (run_id, task_id) = (&handback.run_id, &handback.task_id);
    let bundle = canonical_runner_artifact_ref(runner_id, run_id, task_id, &handback.artifact_id);
    let intent = RunnerDownloadIntent::InternalFetch;
    let bundle = download_remote_artifact_with_intent(&bundle, None, intent)?.output_path;
    let changed = adopt_candidate_bundle(&destination, &bundle, run_id, &handback.candidate)?;
    let hash = snapshot.snapshot_hash.clone();
    Ok(RunnerWorkspaceApplyOutput {
        variant: "workspace_apply",
        command: "runner.workspace.apply",
        local_path: destination.display().to_string(),
        result: ChangeApplyResult::applied(false, hash.clone(), hash, changed, None),
    })
}

/// Fetch `refs/homeboy/candidates/<run_id>` from `bundle`, prove it is exactly
/// `candidate`, then turn the clean `worktree` into the candidate tree on its
/// current branch as uncommitted changes. Returns the changed paths.
pub(super) fn adopt_candidate_bundle(
    worktree: &Path,
    bundle: &Path,
    run_id: &str,
    candidate: &AgentTaskCandidate,
) -> std::result::Result<Vec<String>, CandidateBundleError> {
    use CandidateBundleError::*;
    let sha = candidate.candidate_sha.clone();
    let git =
        |args: &[&str]| git_output(worktree, args).map_err(|e| Unreadable(sha.clone(), e.message));
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(DirtyDestination(status));
    }
    let candidate_ref = format!("refs/homeboy/candidates/{}", sanitize_path_segment(run_id));
    let bundle = bundle.display().to_string();
    git(&[
        "fetch",
        "-q",
        "--no-tags",
        &bundle,
        &format!("+{candidate_ref}:{candidate_ref}"),
    ])?;
    let observed = git(&[
        "rev-parse",
        "--verify",
        &format!("{candidate_ref}^{{commit}}"),
    ])?;
    if observed != sha {
        return Err(CommitMismatch(sha, observed));
    }
    let tree = git(&["rev-parse", &format!("{sha}^{{tree}}")])?;
    if tree != candidate.tree_sha {
        return Err(TreeMismatch(candidate.tree_sha.clone(), tree));
    }
    let head = git(&["rev-parse", "--verify", "HEAD^{commit}"])?;
    if git(&["merge-base", "--is-ancestor", &head, &sha]).is_err() {
        return Err(UnrelatedDestination(candidate.base_sha.clone(), head));
    }
    git(&["read-tree", "-u", "-m", "HEAD", &sha])?;
    git(&["reset", "-q"])?;
    let changed = git(&["diff", "--name-only", "HEAD", &sha])?;
    Ok(changed.lines().map(str::to_string).collect())
}

fn read_lab_patch_artifact(path: &str) -> Result<String> {
    let local_path = if is_retrievable_runner_artifact(path) {
        // Internal: the patch is read once and applied; the operator never sees
        // this cache path (#10585).
        download_remote_artifact_with_intent(path, None, RunnerDownloadIntent::InternalFetch)?
            .output_path
    } else {
        PathBuf::from(path)
    };
    std::fs::read_to_string(&local_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "read runner patch artifact {}",
                local_path.display()
            )),
        )
    })
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn apply_lab_offload_patch_writes_runner_patch_to_local_source() {
        let repo = tempfile::tempdir().expect("repo tempdir");
        git(repo.path(), &["init"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test User"]);
        std::fs::write(repo.path().join("file.txt"), "before\n").expect("seed file");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "base"]);
        let snapshot = homeboy_core::source_snapshot::collect_local(
            "lab",
            repo.path(),
            Some("/srv/homeboy/_lab_workspaces/repo-abc"),
            "lab_offload",
        );
        let artifact_dir = tempfile::tempdir().expect("artifact tempdir");
        let patch_path = artifact_dir.path().join("patch.diff");
        std::fs::write(
            &patch_path,
            "diff --git a/file.txt b/file.txt\nindex 5626abf..f719efd 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n",
        )
        .expect("patch file");
        let exec_output = RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: super::super::RunnerExecMode::Daemon,
            argv: vec!["homeboy".to_string(), "refactor".to_string()],
            remote_cwd: "/srv/homeboy/_lab_workspaces/repo-abc".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: Some(snapshot),
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: Some(serde_json::json!({
                "modified_files": ["file.txt"],
                "patch_artifact_path": patch_path.display().to_string(),
            })),
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        };

        let output = apply_lab_offload_patch(&exec_output)
            .expect("apply patch")
            .expect("patch applied");

        assert_eq!(output.result.modified_files, vec!["file.txt".to_string()]);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("file.txt")).expect("file"),
            "after\n"
        );
    }

    #[test]
    fn apply_lab_offload_patch_writes_returned_homeboy_json_baseline() {
        let repo = tempfile::tempdir().expect("repo tempdir");
        git(repo.path(), &["init"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test User"]);
        std::fs::write(repo.path().join("homeboy.json"), "{\"id\":\"demo\"}\n")
            .expect("seed manifest");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "base"]);
        let snapshot = homeboy_core::source_snapshot::collect_local(
            "lab",
            repo.path(),
            Some("/srv/homeboy/_lab_workspaces/repo-abc"),
            "lab_offload",
        );
        let artifact_dir = tempfile::tempdir().expect("artifact tempdir");
        let patch_path = artifact_dir.path().join("patch.diff");
        std::fs::write(
            &patch_path,
            "diff --git a/homeboy.json b/homeboy.json\nindex 9911a67..6856254 100644\n--- a/homeboy.json\n+++ b/homeboy.json\n@@ -1 +1 @@\n-{\"id\":\"demo\"}\n+{\"id\":\"demo\",\"baselines\":{\"audit\":{\"known_fingerprints\":[\"abc\"]}}}\n",
        )
        .expect("patch file");
        let exec_output = RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: super::super::RunnerExecMode::Daemon,
            argv: vec![
                "homeboy".to_string(),
                "audit".to_string(),
                "--baseline".to_string(),
            ],
            remote_cwd: "/srv/homeboy/_lab_workspaces/repo-abc".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: Some(snapshot),
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: Some(serde_json::json!({
                "modified_files": ["homeboy.json"],
                "patch_artifact_path": patch_path.display().to_string(),
            })),
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        };

        let output = apply_lab_offload_patch(&exec_output)
            .expect("apply patch")
            .expect("patch applied");

        assert_eq!(
            output.result.modified_files,
            vec!["homeboy.json".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("homeboy.json")).expect("manifest"),
            "{\"id\":\"demo\",\"baselines\":{\"audit\":{\"known_fingerprints\":[\"abc\"]}}}\n"
        );
    }

    fn out(repo: &Path, args: &[&str]) -> String {
        git_output(repo, args).unwrap_or_else(|error| panic!("git {args:?}: {}", error.message))
    }

    /// A runner clone that harvested a dirty attempt into
    /// `refs/homeboy/candidates/run-1` and bundled it against the base, and a
    /// controller clone that has only the base.
    struct Fixture {
        _temp: tempfile::TempDir,
        runner: PathBuf,
        controller: PathBuf,
        bundle: PathBuf,
        candidate: AgentTaskCandidate,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let origin = temp.path().join("origin");
        std::fs::create_dir(&origin).expect("origin");
        git(&origin, &["init", "-q", "-b", "main"]);
        git(&origin, &["config", "user.email", "test@example.com"]);
        git(&origin, &["config", "user.name", "Test User"]);
        std::fs::write(origin.join("file.txt"), "base\n").expect("seed");
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-qm", "base"]);
        let [runner, controller] = ["runner", "controller"].map(|name| {
            git(
                temp.path(),
                &["clone", "-q", origin.to_str().unwrap(), name],
            );
            let clone = temp.path().join(name);
            git(&clone, &["config", "user.email", "test@example.com"]);
            git(&clone, &["config", "user.name", "Test User"]);
            clone
        });
        let base = out(&runner, &["rev-parse", "HEAD"]);
        let candidate = harvest(&runner, &base, "agent change\n");
        let bundle = temp.path().join("candidate.bundle");
        bundle_candidate(&runner, &bundle, &base);
        Fixture {
            _temp: temp,
            runner,
            controller,
            bundle,
            candidate,
        }
    }

    /// Commit the dirty attempt worktree as a candidate, as harvest does.
    fn harvest(repo: &Path, base: &str, contents: &str) -> AgentTaskCandidate {
        std::fs::write(repo.join("file.txt"), contents).expect("modify");
        std::fs::write(repo.join("new.txt"), "untracked\n").expect("untracked");
        git(repo, &["add", "--all"]);
        let tree_sha = out(repo, &["write-tree"]);
        let candidate_sha = out(repo, &["commit-tree", &tree_sha, "-p", base, "-m", "c"]);
        git(
            repo,
            &[
                "update-ref",
                "refs/homeboy/candidates/run-1",
                &candidate_sha,
            ],
        );
        AgentTaskCandidate {
            base_sha: base.to_string(),
            candidate_sha,
            tree_sha,
        }
    }

    fn bundle_candidate(repo: &Path, bundle: &Path, base: &str) {
        let bundle = bundle.display().to_string();
        let exclude = format!("^{base}");
        let candidate_ref = "refs/homeboy/candidates/run-1";
        git(
            repo,
            &["bundle", "create", "-q", &bundle, candidate_ref, &exclude],
        );
    }

    /// The tree of the worktree as it stands, read through a scratch index.
    fn worktree_tree(repo: &Path) -> String {
        let index = tempfile::NamedTempFile::new().expect("index");
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                "git read-tree HEAD && git add --all && git write-tree",
            ])
            .env("GIT_INDEX_FILE", index.path())
            .current_dir(repo)
            .output()
            .expect("write tree");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn adopt(fixture: &Fixture, candidate: &AgentTaskCandidate) -> CandidateBundleError {
        adopt_candidate_bundle(&fixture.controller, &fixture.bundle, "run-1", candidate)
            .unwrap_err()
    }

    fn assert_untouched(fixture: &Fixture) {
        let contents = std::fs::read_to_string(fixture.controller.join("file.txt"));
        assert_eq!(contents.expect("file"), "base\n");
        assert!(!fixture.controller.join("new.txt").exists());
    }

    #[test]
    fn base_only_controller_fetches_the_identical_candidate_and_materializes_its_tree() {
        let fixture = fixture();
        let (controller, candidate) = (&fixture.controller, &fixture.candidate);
        assert!(git_output(controller, &["cat-file", "-e", &candidate.candidate_sha]).is_err());
        let branch = out(controller, &["symbolic-ref", "HEAD"]);

        let changed = adopt_candidate_bundle(controller, &fixture.bundle, "run-1", candidate)
            .expect("adopt candidate");

        let pinned = out(controller, &["rev-parse", "refs/homeboy/candidates/run-1"]);
        assert_eq!(pinned, candidate.candidate_sha);
        assert_eq!(worktree_tree(controller), candidate.tree_sha);
        assert_eq!(out(controller, &["symbolic-ref", "HEAD"]), branch);
        assert_eq!(out(controller, &["rev-parse", "HEAD"]), candidate.base_sha);
        assert_eq!(changed, ["file.txt", "new.txt"]);
    }

    #[test]
    fn tampered_bundle_fails_with_typed_commit_mismatch() {
        let fixture = fixture();
        git(&fixture.runner, &["reset", "-q", "--hard"]);
        let base = &fixture.candidate.base_sha;
        let tampered = harvest(&fixture.runner, base, "tampered\n");
        bundle_candidate(&fixture.runner, &fixture.bundle, base);

        let error = adopt(&fixture, &fixture.candidate);

        let expected = fixture.candidate.candidate_sha.clone();
        let observed = tampered.candidate_sha.clone();
        assert_eq!(
            error,
            CandidateBundleError::CommitMismatch(expected.clone(), observed.clone())
        );
        let message = Error::from(error).message;
        assert!(message.contains(&expected) && message.contains(&observed));
        assert_untouched(&fixture);
    }

    #[test]
    fn truncated_bundle_fails_with_typed_unreadable_error() {
        let fixture = fixture();
        let bytes = std::fs::read(&fixture.bundle).expect("bundle");
        std::fs::write(&fixture.bundle, &bytes[..bytes.len() / 2]).expect("truncate");

        let error = adopt(&fixture, &fixture.candidate);

        let CandidateBundleError::Unreadable(expected, _) = &error else {
            panic!("expected an unreadable bundle error, got {error:?}");
        };
        assert_eq!(expected, &fixture.candidate.candidate_sha);
        assert_untouched(&fixture);
    }

    #[test]
    fn recorded_tree_mismatch_fails_with_typed_tree_mismatch() {
        let fixture = fixture();
        let mut recorded = fixture.candidate.clone();
        recorded.tree_sha = out(&fixture.controller, &["rev-parse", "HEAD^{tree}"]);

        let error = adopt(&fixture, &recorded);

        let observed = fixture.candidate.tree_sha.clone();
        assert_eq!(
            error,
            CandidateBundleError::TreeMismatch(recorded.tree_sha, observed)
        );
        assert_untouched(&fixture);
    }

    #[test]
    fn dirty_destination_is_refused_and_left_untouched() {
        let fixture = fixture();
        std::fs::write(fixture.controller.join("operator.txt"), "local work\n").expect("dirty");

        let error = adopt(&fixture, &fixture.candidate);

        assert_eq!(
            error,
            CandidateBundleError::DirtyDestination("?? operator.txt".to_string())
        );
        assert_untouched(&fixture);
    }

    #[test]
    fn destination_that_does_not_precede_the_candidate_is_refused() {
        let fixture = fixture();
        std::fs::write(fixture.controller.join("file.txt"), "diverged\n").expect("diverge");
        git(&fixture.controller, &["commit", "-qam", "diverged"]);
        let head = out(&fixture.controller, &["rev-parse", "HEAD"]);

        let error = adopt(&fixture, &fixture.candidate);

        let base = fixture.candidate.base_sha.clone();
        assert_eq!(
            error,
            CandidateBundleError::UnrelatedDestination(base, head)
        );
        assert!(!fixture.controller.join("new.txt").exists());
    }

    #[test]
    fn lab_promotion_result_hands_back_its_candidate_commit_or_fails() {
        let args =
            ["homeboy", "agent-task", "promote", "/runner/aggregate.json"].map(str::to_string);
        let handback = serde_json::json!({
            "run_id": "run-1",
            "task_id": "task-1",
            "artifact_id": "task-1-attempt-1-candidate-bundle",
            "candidate": { "base_sha": "a", "candidate_sha": "b", "tree_sha": "c" },
        });
        let report = |handback: Value| {
            serde_json::json!({ "data": {
                "status": "gate_failed",
                "provenance": { "lab_offload": { "candidate_handback": handback } },
            }})
            .to_string()
        };

        let parsed = promotion_candidate_handback(&args, &report(handback))
            .expect("handback")
            .expect("gate-failed promotion hands back its candidate");
        assert_eq!(parsed.run_id, "run-1");
        assert_eq!(parsed.candidate.candidate_sha, "b");

        // A result written before the bundle handback is not recoverable.
        let error = promotion_candidate_handback(&args, &report(Value::Null)).unwrap_err();
        assert!(
            error.message.contains("not recoverable"),
            "{}",
            error.message
        );
        let planned = r#"{"data":{"status":"planned"}}"#;
        assert!(promotion_candidate_handback(&args, planned)
            .unwrap()
            .is_none());
    }

    use homeboy_core::test_support::run_git_command as git;
}
