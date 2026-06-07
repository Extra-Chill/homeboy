use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::paths;
use crate::core::source_snapshot::SourceSnapshot;

const PATCH_CAPTURE_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".cache",
];

pub(super) struct BaselineCapture {
    _scratch: ScratchDir,
    path: PathBuf,
}

struct ScratchDir {
    path: PathBuf,
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PatchCaptureReport {
    source_snapshot_id: Option<String>,
    runner_id: String,
    command: Vec<String>,
    remote_path: String,
    modified_files: Vec<String>,
    patch_artifact_id: Option<String>,
    patch_artifact_path: Option<String>,
    dirty_snapshot: bool,
    baseline_missing: bool,
}

struct PatchRunInput<'a> {
    run_id: &'a str,
    runner_id: &'a str,
    cwd: &'a str,
    command: &'a [String],
    source_snapshot: Option<&'a SourceSnapshot>,
    report: &'a PatchCaptureReport,
    patch_artifact_path: Option<&'a Path>,
    artifact_id: &'a str,
    exit_code: i32,
}

pub(super) fn capture_baseline(cwd: &str) -> Result<BaselineCapture> {
    let cwd_path = Path::new(cwd);
    if !cwd_path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "patch capture requires an existing directory baseline",
            Some(cwd.to_string()),
            None,
        ));
    }
    let scratch = create_scratch_dir("baseline")?;
    let baseline_path = scratch.path.join("baseline");
    copy_dir_filtered(cwd_path, &baseline_path)?;
    Ok(BaselineCapture {
        _scratch: scratch,
        path: baseline_path,
    })
}

pub(super) fn capture_patch_report(
    job_id: uuid::Uuid,
    runner_id: &str,
    cwd: &str,
    command: &[String],
    source_snapshot: Option<&SourceSnapshot>,
    baseline: &BaselineCapture,
    exit_code: i32,
) -> Result<PatchCaptureReport> {
    let after_scratch = create_scratch_dir("after")?;
    let after_path = after_scratch.path.join("after");
    copy_dir_filtered(Path::new(cwd), &after_path)?;

    let patch = normalized_no_index_diff(&baseline.path, &after_path)?;
    let modified_files = no_index_modified_files(&baseline.path, &after_path)?;
    let run_id = format!("runner-exec-{job_id}");
    let artifact_id = format!("runner-fix-patch-{job_id}");
    let patch_artifact_path = if patch.trim().is_empty() {
        None
    } else {
        Some(write_patch_artifact(&run_id, &artifact_id, &patch)?)
    };
    let patch_artifact_path_string = patch_artifact_path
        .as_ref()
        .map(|path| path.display().to_string());
    let report = PatchCaptureReport {
        source_snapshot_id: source_snapshot.map(|snapshot| snapshot.snapshot_hash.clone()),
        runner_id: runner_id.to_string(),
        command: command.to_vec(),
        remote_path: cwd.to_string(),
        modified_files,
        patch_artifact_id: patch_artifact_path.as_ref().map(|_| artifact_id.clone()),
        patch_artifact_path: patch_artifact_path_string,
        dirty_snapshot: source_snapshot
            .map(|snapshot| snapshot.dirty)
            .unwrap_or(false),
        baseline_missing: false,
    };
    persist_patch_run(PatchRunInput {
        run_id: &run_id,
        runner_id,
        cwd,
        command,
        source_snapshot,
        report: &report,
        patch_artifact_path: patch_artifact_path.as_deref(),
        artifact_id: &artifact_id,
        exit_code,
    })?;
    Ok(report)
}

fn create_scratch_dir(label: &str) -> Result<ScratchDir> {
    let path = paths::artifact_root()?
        .join("_scratch")
        .join(format!("patch-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create scratch directory {}", path.display())),
        )
    })?;
    Ok(ScratchDir { path })
}

fn write_patch_artifact(run_id: &str, artifact_id: &str, patch: &str) -> Result<PathBuf> {
    let path = paths::artifact_root()?
        .join(run_id)
        .join(format!("{artifact_id}.diff"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    fs::write(&path, patch).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("write patch artifact {}", path.display())),
        )
    })?;
    Ok(path)
}

fn persist_patch_run(input: PatchRunInput<'_>) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let now = chrono::Utc::now().to_rfc3339();
    let run = RunRecord {
        id: input.run_id.to_string(),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: now.clone(),
        finished_at: Some(now.clone()),
        status: if input.exit_code == 0 { "pass" } else { "fail" }.to_string(),
        command: Some(input.command.join(" ")),
        cwd: Some(input.cwd.to_string()),
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: input
            .source_snapshot
            .and_then(|snapshot| snapshot.git_sha.clone()),
        rig_id: None,
        metadata_json: json!({
            "lab": {
                "runner_id": input.runner_id,
                "source_snapshot": input.source_snapshot,
                "patch": input.report,
            }
        }),
    };
    if store.get_run(input.run_id)?.is_none() {
        store.import_run(&run)?;
    }
    if let Some(path) = input.patch_artifact_path {
        let bytes = fs::read(path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        let artifact = ArtifactRecord {
            id: input.artifact_id.to_string(),
            run_id: input.run_id.to_string(),
            kind: "lab_fix_patch".to_string(),
            artifact_type: "file".to_string(),
            path: path.display().to_string(),
            url: None,
            sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
            size_bytes: i64::try_from(bytes.len()).ok(),
            mime: Some("text/x-diff".to_string()),
            metadata_json: serde_json::json!({}),
            created_at: now,
        };
        if store.get_artifact(input.artifact_id)?.is_none() {
            store.import_artifact(&artifact)?;
        }
    }
    Ok(())
}

fn normalized_no_index_diff(baseline: &Path, after: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--no-index", "--binary", "--"])
        .arg(baseline)
        .arg(after)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("run git diff --no-index".to_string()))
        })?;
    let code = output.status.code().unwrap_or(1);
    if code > 1 {
        return Err(Error::internal_unexpected(format!(
            "git diff --no-index failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(normalize_patch_paths(
        &String::from_utf8_lossy(&output.stdout),
        baseline,
        after,
    ))
}

fn no_index_modified_files(baseline: &Path, after: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--no-index", "--name-only", "--"])
        .arg(baseline)
        .arg(after)
        .output()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("run git diff --name-only".to_string()),
            )
        })?;
    let code = output.status.code().unwrap_or(1);
    if code > 1 {
        return Err(Error::internal_unexpected(format!(
            "git diff --name-only failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut files = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let path = Path::new(line);
        let relative = path
            .strip_prefix(after)
            .or_else(|_| path.strip_prefix(baseline))
            .unwrap_or(path)
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        if !relative.is_empty() {
            files.insert(relative);
        }
    }
    Ok(files.into_iter().collect())
}

fn normalize_patch_paths(patch: &str, baseline: &Path, after: &Path) -> String {
    let baseline = baseline.to_string_lossy();
    let after = after.to_string_lossy();
    patch
        .replace(&format!("a/{baseline}"), "a")
        .replace(&format!("b/{after}"), "b")
        .replace(baseline.as_ref(), "a")
        .replace(after.as_ref(), "b")
}

fn copy_dir_filtered(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create {}", target.display())),
        )
    })?;
    for entry in fs::read_dir(source).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", source.display())))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(err.to_string(), Some("read directory entry".to_string()))
        })?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if PATCH_CAPTURE_EXCLUDES.contains(&name_str) {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(&name);
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("read metadata {}", source_path.display())),
            )
        })?;
        if metadata.is_dir() {
            copy_dir_filtered(&source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("copy {}", source_path.display())),
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HomeGuard;

    #[test]
    fn capture_baseline_filters_large_generated_directories() {
        let _home = HomeGuard::new();
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("file.txt"), "tracked\n").expect("file");
        fs::create_dir(workspace.path().join("target")).expect("target dir");
        fs::write(
            workspace.path().join("target").join("ignored.txt"),
            "ignored\n",
        )
        .expect("ignored file");

        let baseline = capture_baseline(&workspace.path().display().to_string()).expect("baseline");

        assert!(baseline.path.join("file.txt").exists());
        assert!(!baseline.path.join("target").exists());
    }

    #[test]
    fn capture_patch_report_records_diff_artifact() {
        let _home = HomeGuard::new();
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("file.txt"), "before\n").expect("before");
        let baseline = capture_baseline(&workspace.path().display().to_string()).expect("baseline");
        fs::write(workspace.path().join("file.txt"), "after\n").expect("after");
        let job_id = uuid::Uuid::new_v4();
        let command = vec!["sh".to_string(), "-c".to_string(), "true".to_string()];

        let report = capture_patch_report(
            job_id,
            "lab-local",
            &workspace.path().display().to_string(),
            &command,
            None,
            &baseline,
            0,
        )
        .expect("patch report");

        assert_eq!(report.modified_files, vec!["file.txt".to_string()]);
        let artifact_path = report.patch_artifact_path.expect("artifact path");
        let patch_body = fs::read_to_string(artifact_path).expect("patch body");
        assert!(patch_body.contains("-before"));
        assert!(patch_body.contains("+after"));
    }
}
