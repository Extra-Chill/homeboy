use homeboy_engine_primitives::content_hash;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use homeboy_core::{Error, Result};

use super::types::AgentTaskPromotionOptions;

pub(crate) struct CommittedChangesPatch {
    pub(crate) base_ref: String,
    pub(crate) candidate: String,
    pub(crate) historical_task_base: Option<String>,
    pub(crate) adoption_merge: Option<AdoptionMergeProof>,
    pub(crate) patch_path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) commit_range: String,
    pub(crate) commits: Vec<Value>,
}

/// The graph roles authenticated when adoption selects a merge commit. Keeping
/// these immutable IDs in promotion evidence makes the candidate-only delta
/// independently reviewable without relying on branch names or mutable refs.
#[derive(Clone)]
pub(crate) struct AdoptionMergeProof {
    pub(crate) candidate_parent: String,
    pub(crate) resolved_base_parent: String,
    pub(crate) candidate_delta_base: String,
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
    if options.candidate_ref.is_some() {
        ensure_clean_source(worktree_path)?;
    }
    let candidate = resolve_candidate(worktree_path, options.candidate_ref.as_deref())?;
    if options.candidate_ref.is_some() {
        let head = git_stdout(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if candidate != head.trim() {
            return Err(Error::validation_invalid_argument(
                "candidate_ref",
                "candidate revision must equal the recorded source worktree HEAD",
                Some(candidate),
                None,
            ));
        }
    }

    let (base_ref, historical_task_base, adoption_merge) = if options.candidate_ref.is_some() {
        resolve_adoption_candidate_base(
            worktree_path,
            &candidate,
            options.task_base_sha.as_deref(),
        )?
    } else {
        let Some(base_ref) = resolve_committed_changes_base(
            worktree_path,
            options.task_base_sha.as_deref(),
            options.base_ref.as_deref(),
        )?
        else {
            return Ok(None);
        };
        (base_ref, None, None)
    };
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
    // The merge itself incorporates the advanced base. Promotion must project
    // only the authenticated candidate-side commit, not that base-side tree.
    let delta_candidate = adoption_merge
        .as_ref()
        .map(|proof| proof.candidate_parent.as_str())
        .unwrap_or(&candidate);
    let changed_files = git_lines(
        worktree_path,
        &["diff", "--name-only", &base_ref, delta_candidate],
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
            delta_candidate,
        ],
    )?;
    if patch.trim().is_empty() {
        return Ok(None);
    }
    let commit_range = format!("{base_ref}..{delta_candidate}");
    let commits = committed_change_evidence(worktree_path, &commit_range)?;
    if commits.is_empty() {
        return Ok(None);
    }
    let sha256 = content_hash::sha256_hex(patch.as_bytes());
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
        candidate,
        historical_task_base,
        adoption_merge,
        patch_path,
        sha256,
        commit_range,
        commits,
    }))
}

/// Explicit adoption is scoped to the immutable candidate delta, not the task's
/// historical workspace base. A candidate may be a normal one-parent commit or
/// a two-parent merge that incorporates the verified base after it advanced.
fn resolve_adoption_candidate_base(
    cwd: &Path,
    candidate: &str,
    task_base_sha: Option<&str>,
) -> Result<(String, Option<String>, Option<AdoptionMergeProof>)> {
    let parents = git_stdout(cwd, &["rev-list", "--parents", "-n", "1", candidate])?;
    let mut identities = parents.split_whitespace();
    let _commit = identities.next();
    let parents = identities.map(str::to_string).collect::<Vec<_>>();
    let historical_task_base = task_base_sha
        .filter(|base| !base.trim().is_empty())
        .map(|base| {
            git_stdout(
                cwd,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
            )
            .map(|base| base.trim().to_string())
        })
        .transpose()?;
    match parents.as_slice() {
        [parent] => {
            validate_adoption_parent_lineage(
                cwd,
                candidate,
                parent,
                historical_task_base.as_deref(),
            )?;
            Ok((parent.clone(), historical_task_base, None))
        }
        [candidate_parent, resolved_base_parent] => {
            let candidate_parents = git_commit_parents(cwd, candidate_parent)?;
            let [candidate_delta_base] = candidate_parents.as_slice() else {
                return Err(adoption_graph_error(
                    candidate,
                    "merge candidate's first parent must have exactly one parent",
                ));
            };
            let historical = historical_task_base.as_deref().ok_or_else(|| {
                adoption_graph_error(
                    candidate,
                    "merge candidate requires the recorded immutable task base",
                )
            })?;
            for (role, revision) in [
                ("candidate delta base", candidate_delta_base.as_str()),
                ("candidate-side parent", candidate_parent.as_str()),
                ("resolved base parent", resolved_base_parent.as_str()),
            ] {
                if !is_ancestor(cwd, historical, revision)? {
                    return Err(adoption_graph_error(candidate, &format!(
                        "merge candidate's {role} regresses or is unrelated to the recorded task base"
                    )));
                }
            }
            if is_ancestor(cwd, resolved_base_parent, candidate_parent)?
                || is_ancestor(cwd, candidate_parent, resolved_base_parent)?
            {
                return Err(adoption_graph_error(
                    candidate,
                    "merge parents do not represent distinct candidate and advanced-base lineages",
                ));
            }
            Ok((
                candidate_delta_base.clone(),
                historical_task_base,
                Some(AdoptionMergeProof {
                    candidate_parent: candidate_parent.clone(),
                    resolved_base_parent: resolved_base_parent.clone(),
                    candidate_delta_base: candidate_delta_base.clone(),
                }),
            ))
        }
        _ => Err(adoption_graph_error(
            candidate,
            "adopted candidate must have one parent or exactly two authenticated merge parents",
        )),
    }
}

fn validate_adoption_parent_lineage(
    cwd: &Path,
    candidate: &str,
    parent: &str,
    historical_task_base: Option<&str>,
) -> Result<()> {
    if !is_ancestor(cwd, parent, candidate)? {
        return Err(adoption_graph_error(
            candidate,
            "adopted candidate is not descended from its immutable parent base",
        ));
    }
    if let Some(historical) = historical_task_base.filter(|historical| *historical != candidate) {
        if !is_ancestor(cwd, historical, parent)? {
            return Err(Error::validation_invalid_argument(
                "task_base_sha",
                "recorded task base is unrelated to the adopted candidate parent; refusing ambiguous adoption provenance",
                Some(historical.to_string()),
                None,
            ));
        }
    }
    Ok(())
}

fn git_commit_parents(cwd: &Path, revision: &str) -> Result<Vec<String>> {
    let output = git_stdout(cwd, &["rev-list", "--parents", "-n", "1", revision])?;
    Ok(output
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect())
}

fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(cwd)
        .status()
        .map(|status| status.success())
        .map_err(|error| Error::git_command_failed(error.to_string()))
}

fn adoption_graph_error(candidate: &str, message: &str) -> Error {
    Error::validation_invalid_argument("candidate_ref", message, Some(candidate.to_string()), None)
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

pub(crate) fn resolve_candidate_revision(cwd: &Path, requested: &str) -> Result<String> {
    resolve_candidate(cwd, Some(requested))
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
