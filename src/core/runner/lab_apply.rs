use std::path::PathBuf;

use crate::core::change_artifact::{ChangeArtifact, ChangePatch, CHANGE_ARTIFACT_SCHEMA};
use crate::core::{Error, Result};

use super::{
    apply_change_artifact, download_remote_artifact, is_retrievable_runner_artifact,
    RunnerExecOutput, RunnerWorkspaceApplyOutput,
};

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
            format: "unified_diff".to_string(),
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
        download_remote_artifact(path, None)?.output_path
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
    use std::path::Path;

    use crate::core::source_snapshot::SourceSnapshot;

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
        let snapshot = SourceSnapshot::collect_local(
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
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: Some(serde_json::json!({
                "modified_files": ["file.txt"],
                "patch_artifact_path": patch_path.display().to_string(),
            })),
            metrics: None,
            capture: None,
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
        let snapshot = SourceSnapshot::collect_local(
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
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: Some(serde_json::json!({
                "modified_files": ["homeboy.json"],
                "patch_artifact_path": patch_path.display().to_string(),
            })),
            metrics: None,
            capture: None,
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

    fn git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
