use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use homeboy_core::{Error, Result};

use super::types::AgentTaskPromotionOptions;

pub(crate) struct CommittedChangesPatch {
    pub(crate) base_ref: String,
    pub(crate) patch_path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) commit_range: String,
    pub(crate) commits: Vec<Value>,
}

pub(crate) fn committed_changes_patch(
    options: &AgentTaskPromotionOptions,
) -> Result<Option<CommittedChangesPatch>> {
    let Some(worktree_path) = options.source_worktree_path.as_deref() else {
        return Ok(None);
    };
    if !worktree_path.is_dir() {
        return Ok(None);
    }
    let Some(base_ref) = resolve_committed_changes_base(
        worktree_path,
        options.task_base_sha.as_deref(),
        options.base_ref.as_deref(),
    )?
    else {
        return Ok(None);
    };
    if options.candidate_ref.is_some() {
        ensure_clean_source(worktree_path)?;
    }
    let candidate = resolve_candidate(worktree_path, options.candidate_ref.as_deref())?;
    if options.candidate_ref.is_some() {
        let head = git_stdout(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if candidate != head.trim() {
            return Err(Error::validation_invalid_argument(
                "candidate_ref",
                "candidate revision does not match the recorded source worktree HEAD",
                Some(candidate),
                None,
            ));
        }
    }
    let is_ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", &base_ref, &candidate])
        .current_dir(worktree_path)
        .status()
        .map_err(|error| Error::git_command_failed(error.to_string()))?
        .success();
    if !is_ancestor {
        return Err(Error::validation_invalid_argument(
            "candidate_ref",
            "candidate revision is not descended from the recorded task base",
            Some(candidate),
            None,
        ));
    }
    let changed_files = git_lines(
        worktree_path,
        &["diff", "--name-only", &base_ref, &candidate],
    )?;
    if changed_files.is_empty() {
        return Ok(None);
    }
    let patch = git_stdout(
        worktree_path,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            &base_ref,
            &candidate,
        ],
    )?;
    if patch.trim().is_empty() {
        return Ok(None);
    }
    let commit_range = format!("{base_ref}..{candidate}");
    let commits = committed_change_evidence(worktree_path, &commit_range)?;
    if commits.is_empty() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    hasher.update(patch.as_bytes());
    let sha256 = format!("{:x}", hasher.finalize());
    let patch_path = committed_changes_patch_path(options, &sha256)?;
    std::fs::write(&patch_path, patch.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "write committed changes promotion patch {}",
                patch_path.display()
            )),
        )
    })?;
    Ok(Some(CommittedChangesPatch {
        base_ref,
        patch_path,
        sha256,
        commit_range,
        commits,
    }))
}

fn ensure_clean_source(cwd: &Path) -> Result<()> {
    let status = git_stdout(cwd, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "source_worktree",
        "candidate source worktree is dirty; refusing to derive an ambiguous commit candidate",
        Some(cwd.display().to_string()),
        None,
    ))
}

fn resolve_candidate(cwd: &Path, requested: Option<&str>) -> Result<String> {
    let candidate = requested.unwrap_or("HEAD");
    git_stdout(
        cwd,
        &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
    )
    .map(|value| value.trim().to_string())
    .map_err(|_| {
        Error::validation_invalid_argument(
            "candidate_ref",
            "candidate revision is not present in the recorded source repository",
            Some(candidate.to_string()),
            None,
        )
    })
}

fn committed_changes_patch_path(
    options: &AgentTaskPromotionOptions,
    sha256: &str,
) -> Result<PathBuf> {
    if let Some(parent) = options.source_path.as_deref().and_then(Path::parent) {
        return Ok(parent.join(format!("committed-changes-{sha256}.patch")));
    }
    let dir = std::env::temp_dir().join("homeboy-agent-task-promotions");
    std::fs::create_dir_all(&dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create committed changes promotion artifact directory {}",
                dir.display()
            )),
        )
    })?;
    Ok(dir.join(format!("committed-changes-{sha256}.patch")))
}

fn resolve_committed_changes_base(
    cwd: &Path,
    task_base_sha: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>> {
    if let Some(base) = task_base_sha.filter(|value| !value.trim().is_empty()) {
        let base = git_stdout(
            cwd,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )?;
        let is_ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", base.trim(), "HEAD"])
            .current_dir(cwd)
            .status()
            .map_err(|error| Error::git_command_failed(error.to_string()))?
            .success();
        if !is_ancestor {
            return Err(Error::validation_invalid_argument(
                "task_base_sha",
                "recorded task base is not an ancestor of the source workspace HEAD; refusing to promote unrelated or pre-existing commits",
                Some(base.trim().to_string()),
                None,
            ));
        }
        return Ok(Some(base.trim().to_string()));
    }
    let mut candidates = Vec::new();
    if let Some(requested) = requested.filter(|value| !value.trim().is_empty()) {
        candidates.push(requested.to_string());
        if !requested.contains('/') {
            candidates.push(format!("origin/{requested}"));
        }
    }
    candidates.push("@{upstream}".to_string());
    for candidate in candidates {
        if git_stdout(
            cwd,
            &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
        )
        .is_ok()
        {
            let merge_base = git_stdout(cwd, &["merge-base", &candidate, "HEAD"])?;
            return Ok(Some(merge_base.trim().to_string()));
        }
    }
    Ok(None)
}

fn committed_change_evidence(cwd: &Path, range: &str) -> Result<Vec<Value>> {
    let output = git_stdout(
        cwd,
        &["log", "--reverse", "--format=%H%x1f%an%x1f%ae%x1f%s", range],
    )?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            Some(json!({
                "sha": fields.next()?,
                "author_name": fields.next()?,
                "author_email": fields.next()?,
                "subject": fields.next()?,
            }))
        })
        .collect())
}

fn git_lines(cwd: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git_stdout(cwd, args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
