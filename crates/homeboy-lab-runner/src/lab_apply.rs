use std::path::PathBuf;

use homeboy_core::change_artifact::{
    ChangeArtifact, ChangePatch, CHANGE_ARTIFACT_SCHEMA, UNIFIED_DIFF_PATCH_FORMAT,
};
use homeboy_core::{Error, Result};

use homeboy_core::runner_download_cache::RunnerDownloadIntent;

use super::{
    apply_change_artifact, download_remote_artifact_with_intent, is_retrievable_runner_artifact,
    RunnerExecOutput, RunnerWorkspaceApplyOutput,
};

pub(super) fn apply_lab_candidate(exec: &RunnerExecOutput) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(&exec.stdout)
        .map_err(|e| Error::internal_json(e.to_string(), None))?;
    let report = value.get("data").unwrap_or(&value);
    if !matches!(report["status"].as_str(), Some("applied" | "gate_failed")) {
        return Ok(());
    }
    let candidate = serde_json::from_value(report["provenance"]["candidate_commit"].clone())
        .map_err(|_| {
            Error::validation_invalid_argument(
                "source.candidate",
                "non-recoverable: Lab promotion has no immutable candidate",
                None,
                None,
            )
            .with_retryable(false)
        })?;
    let run_id = report["source"]["run_id"]
        .as_str()
        .ok_or_else(|| Error::internal_unexpected("Lab candidate has no run id"))?;
    // The run-plan handback already fetched and verified this candidate. Never
    // resolve a runner-only SHA here; promotion is only materialization.
    let root = exec
        .source_snapshot
        .as_ref()
        .and_then(|s| s.local_path.as_deref())
        .ok_or_else(|| Error::internal_unexpected("Lab candidate has no destination"))?;
    homeboy_agents::agent_task_promotion::verify_candidate_ref(
        std::path::Path::new(root),
        run_id,
        &candidate,
    )?;
    homeboy_agents::agent_task_promotion::materialize_candidate_tree(
        std::path::Path::new(root),
        &candidate,
    )?;
    Ok(())
}

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
    fn candidate_handback_uses_verified_ref_not_runner_patch_or_changed_files() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init"]);
        git(repo.path(), &["config", "user.email", "t@t"]);
        git(repo.path(), &["config", "user.name", "T"]);
        std::fs::write(repo.path().join("file"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "base"]);
        let output = |args: &[&str]| {
            homeboy_core::git::run_git(repo.path(), args, "test")
                .unwrap()
                .trim()
                .to_string()
        };
        let base = output(&["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("file"), "candidate").unwrap();
        std::fs::write(repo.path().join("new"), "new").unwrap();
        git(repo.path(), &["add", "."]);
        let tree = output(&["write-tree"]);
        let sha = output(&["commit-tree", &tree, "-p", &base, "-m", "candidate"]);
        git(repo.path(), &["reset", "--hard", "HEAD"]);
        git(
            repo.path(),
            &["update-ref", "refs/homeboy/candidates/run-1", &sha],
        );
        let mut exec = RunnerExecOutput {
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
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
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
        exec.source_snapshot = Some(homeboy_core::source_snapshot::collect_local(
            "lab",
            repo.path(),
            Some("/runner"),
            "lab_offload",
        ));
        exec.stdout = serde_json::json!({"status":"gate_failed", "source":{"run_id":"run-1"}, "provenance":{"candidate_commit":{"base_sha":base,"candidate_sha":sha,"tree_sha":tree}}, "changed_files":[]}).to_string();
        apply_lab_candidate(&exec).unwrap();
        assert_eq!(output(&["write-tree"]), tree);
        assert_eq!(output(&["rev-parse", "HEAD"]), base);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("new")).unwrap(),
            "new"
        );
    }

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

    use homeboy_core::test_support::run_git_command as git;
}
