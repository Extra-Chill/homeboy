use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::core::engine::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};
use crate::core::error::{Error, Result};
use crate::core::source_snapshot::SourceSnapshot;

#[derive(Debug, Clone)]
pub struct RunnerWorkspaceApplyOptions {
    pub input: String,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkspaceApplyStatus {
    Applied,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceApplyOutput {
    pub command: &'static str,
    pub local_path: String,
    pub apply_status: RunnerWorkspaceApplyStatus,
    pub force: bool,
    pub expected_snapshot_hash: String,
    pub current_snapshot_hash: String,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LabPatchApplyInput {
    source_snapshot: SourceSnapshot,
    #[serde(default)]
    patch: Option<LabPatch>,
    #[serde(default)]
    delta: Option<LabDelta>,
}

#[derive(Debug, Deserialize)]
struct LabPatch {
    #[serde(default)]
    format: Option<String>,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LabDelta {
    files: Vec<LabDeltaFile>,
}

#[derive(Debug, Deserialize)]
struct LabDeltaFile {
    path: String,
    #[serde(default)]
    content_base64: Option<String>,
    #[serde(default)]
    delete: bool,
}

pub fn apply_workspace_patch(
    options: RunnerWorkspaceApplyOptions,
) -> Result<(RunnerWorkspaceApplyOutput, i32)> {
    let input = read_apply_input(&options.input)?;
    let local_path = local_source_path(&input.source_snapshot)?;
    let current = SourceSnapshot::collect_local(
        &input.source_snapshot.runner_id,
        &local_path,
        input.source_snapshot.remote_path.as_deref(),
        &input.source_snapshot.sync_mode,
    );

    if current.snapshot_hash != input.source_snapshot.snapshot_hash && !options.force {
        return Err(Error::validation_invalid_argument(
            "source_snapshot",
            "local source worktree has drifted since the Lab snapshot; rerun the Lab job from a fresh snapshot or pass --force to apply explicitly",
            Some(local_path.display().to_string()),
            Some(vec![format!(
                "expected {}, current {}",
                input.source_snapshot.snapshot_hash, current.snapshot_hash
            )]),
        ));
    }

    let modified_files = match (input.patch, input.delta) {
        (Some(patch), None) => apply_unified_patch(&local_path, patch)?,
        (None, Some(delta)) => apply_delta(&local_path, delta)?,
        (Some(_), Some(_)) => {
            return Err(Error::validation_invalid_argument(
                "input",
                "Lab apply input must contain either patch or delta, not both",
                None,
                None,
            ));
        }
        (None, None) => {
            return Err(Error::validation_invalid_argument(
                "input",
                "Lab apply input must contain patch or delta",
                None,
                None,
            ));
        }
    };

    Ok((
        RunnerWorkspaceApplyOutput {
            command: "runner.workspace.apply",
            local_path: local_path.display().to_string(),
            apply_status: RunnerWorkspaceApplyStatus::Applied,
            force: options.force,
            expected_snapshot_hash: input.source_snapshot.snapshot_hash,
            current_snapshot_hash: current.snapshot_hash,
            modified_files,
        },
        0,
    ))
}

fn read_apply_input(path: &str) -> Result<LabPatchApplyInput> {
    let contents = fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(format!("read {path}"))))?;
    serde_json::from_str(&contents).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse Lab apply input".to_string()))
    })
}

fn local_source_path(snapshot: &SourceSnapshot) -> Result<PathBuf> {
    let path = snapshot.local_path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "source_snapshot.local_path",
            "Lab apply requires the local source worktree from the source snapshot",
            Some(snapshot.snapshot_hash.clone()),
            None,
        )
    })?;
    let path = shellexpand::tilde(path).to_string();
    let path = Path::new(&path);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "source_snapshot.local_path",
            "local source worktree does not exist",
            Some(path.display().to_string()),
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("canonicalize source snapshot path".to_string()),
        )
    })
}

fn apply_unified_patch(local_path: &Path, patch: LabPatch) -> Result<Vec<String>> {
    let format = patch.format.as_deref().unwrap_or("unified_diff");
    if format != "unified_diff" {
        return Err(Error::validation_invalid_argument(
            "patch.format",
            "only unified_diff Lab patches are supported",
            Some(format.to_string()),
            None,
        ));
    }
    let modified_files = git_apply_numstat(local_path, &patch.content)?;
    run_git_with_stdin(local_path, &["apply", "--check", "-"], &patch.content)?;
    run_git_with_stdin(local_path, &["apply", "-"], &patch.content)?;
    Ok(modified_files)
}

fn git_apply_numstat(local_path: &Path, patch: &str) -> Result<Vec<String>> {
    let output = run_git_with_stdin(local_path, &["apply", "--numstat", "-"], patch)?;
    let mut files = output
        .lines()
        .filter_map(|line| line.rsplit('\t').next())
        .filter(|path| !path.is_empty())
        .map(|path| path.to_string())
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    Ok(files)
}

fn run_git_with_stdin(local_path: &Path, args: &[&str], stdin: &str) -> Result<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(local_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run git".to_string())))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| Error::internal_unexpected("git stdin unavailable"))?
        .write_all(stdin.as_bytes())
        .map_err(|err| Error::internal_io(err.to_string(), Some("write git stdin".to_string())))?;
    let output = wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES)
        .map_err(|err| Error::internal_io(err.to_string(), Some("wait for git".to_string())))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(Error::git_command_failed(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn apply_delta(local_path: &Path, delta: LabDelta) -> Result<Vec<String>> {
    if delta.files.is_empty() {
        return Err(Error::validation_invalid_argument(
            "delta.files",
            "delta must include at least one file",
            None,
            None,
        ));
    }
    let mut modified = Vec::new();
    for file in delta.files {
        let target = safe_join(local_path, &file.path)?;
        if file.delete {
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(Error::internal_io(
                        err.to_string(),
                        Some(format!("delete {}", target.display())),
                    ));
                }
            }
        } else {
            let encoded = file.content_base64.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "delta.files.content_base64",
                    "delta file writes require content_base64 unless delete is true",
                    Some(file.path.clone()),
                    None,
                )
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|err| {
                    Error::internal_json(err.to_string(), Some("decode delta file".to_string()))
                })?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
            fs::write(&target, bytes).map_err(|err| {
                Error::internal_io(err.to_string(), Some(format!("write {}", target.display())))
            })?;
        }
        modified.push(file.path);
    }
    modified.sort();
    modified.dedup();
    Ok(modified)
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::validation_invalid_argument(
            "delta.files.path",
            "delta paths must be relative and stay inside the source worktree",
            Some(relative.to_string()),
            None,
        ));
    }
    Ok(root.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source_snapshot::SourceSnapshot;

    #[test]
    fn test_apply_workspace_patch() {
        let repo = git_repo();
        let snapshot =
            SourceSnapshot::collect_local("lab", repo.path(), Some("/lab/repo"), "snapshot");
        let input_dir = tempfile::tempdir().expect("input tempdir");
        let input = input_dir.path().join("lab-patch.json");
        fs::write(
            &input,
            serde_json::json!({
                "source_snapshot": snapshot,
                "patch": {
                    "format": "unified_diff",
                    "content": "diff --git a/file.txt b/file.txt\nindex 5626abf..f719efd 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n"
                }
            })
            .to_string(),
        )
        .expect("write input");

        let (output, exit_code) = apply_workspace_patch(RunnerWorkspaceApplyOptions {
            input: input.display().to_string(),
            force: false,
        })
        .expect("apply patch");

        assert_eq!(exit_code, 0);
        assert_eq!(output.apply_status, RunnerWorkspaceApplyStatus::Applied);
        assert_eq!(output.modified_files, vec!["file.txt".to_string()]);
        assert_eq!(
            fs::read_to_string(repo.path().join("file.txt")).unwrap(),
            "after\n"
        );
    }

    #[test]
    fn rejects_local_drift_without_force() {
        let repo = git_repo();
        let snapshot =
            SourceSnapshot::collect_local("lab", repo.path(), Some("/lab/repo"), "snapshot");
        fs::write(repo.path().join("other.txt"), "local drift\n").expect("drift");
        let input_dir = tempfile::tempdir().expect("input tempdir");
        let input = input_dir.path().join("lab-patch.json");
        fs::write(
            &input,
            serde_json::json!({
                "source_snapshot": snapshot,
                "patch": {
                    "format": "unified_diff",
                    "content": "diff --git a/file.txt b/file.txt\nindex 5626abf..f719efd 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n"
                }
            })
            .to_string(),
        )
        .expect("write input");

        let err = apply_workspace_patch(RunnerWorkspaceApplyOptions {
            input: input.display().to_string(),
            force: false,
        })
        .expect_err("drift rejects");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("drifted"));
        assert_eq!(
            fs::read_to_string(repo.path().join("file.txt")).unwrap(),
            "before\n"
        );
    }

    #[test]
    fn applies_delta_with_force_after_explicit_drift_acknowledgement() {
        let repo = git_repo();
        let snapshot =
            SourceSnapshot::collect_local("lab", repo.path(), Some("/lab/repo"), "snapshot");
        fs::write(repo.path().join("other.txt"), "local drift\n").expect("drift");
        let input_dir = tempfile::tempdir().expect("input tempdir");
        let input = input_dir.path().join("lab-delta.json");
        fs::write(
            &input,
            serde_json::json!({
                "source_snapshot": snapshot,
                "delta": {
                    "files": [{
                        "path": "nested/file.txt",
                        "content_base64": "ZGVsdGEK"
                    }]
                }
            })
            .to_string(),
        )
        .expect("write input");

        let (output, _) = apply_workspace_patch(RunnerWorkspaceApplyOptions {
            input: input.display().to_string(),
            force: true,
        })
        .expect("force delta");

        assert!(output.force);
        assert_eq!(output.modified_files, vec!["nested/file.txt".to_string()]);
        assert_eq!(
            fs::read_to_string(repo.path().join("nested/file.txt")).unwrap(),
            "delta\n"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("other.txt")).unwrap(),
            "local drift\n"
        );
    }

    #[test]
    fn rejects_delta_path_traversal() {
        let repo = git_repo();
        let snapshot =
            SourceSnapshot::collect_local("lab", repo.path(), Some("/lab/repo"), "snapshot");
        let input_dir = tempfile::tempdir().expect("input tempdir");
        let input = input_dir.path().join("lab-delta.json");
        fs::write(
            &input,
            serde_json::json!({
                "source_snapshot": snapshot,
                "delta": { "files": [{ "path": "../outside", "content_base64": "eA==" }] }
            })
            .to_string(),
        )
        .expect("write input");

        let err = apply_workspace_patch(RunnerWorkspaceApplyOptions {
            input: input.display().to_string(),
            force: false,
        })
        .expect_err("path traversal rejects");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("delta paths"));
    }

    fn git_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("repo tempdir");
        git(repo.path(), &["init"]);
        fs::write(repo.path().join("file.txt"), "before\n").expect("seed file");
        git(repo.path(), &["add", "file.txt"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Tests",
                "-c",
                "user.email=homeboy@example.com",
                "commit",
                "-m",
                "seed",
            ],
        );
        repo
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
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
