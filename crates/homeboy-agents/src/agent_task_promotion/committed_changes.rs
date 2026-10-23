use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use homeboy_core::{Error, Result};

use super::types::AgentTaskPromotionRequest;

/// An explicitly adopted revision (`candidate_ref`) as the Cook candidate
/// commit (#16303), with the lineage evidence that authenticated it.
pub(crate) struct AdoptedCandidate {
    pub(crate) candidate: crate::agent_task::AgentTaskCandidate,
    pub(crate) provenance: Value,
}

/// Resolve the adopted revision to a candidate commit. Its base is the
/// immutable base the lineage validation authenticates against the recorded
/// task base: the candidate's parent, or the resolved base parent of a merge.
pub(crate) fn adopted_candidate(options: &AgentTaskPromotionRequest) -> Result<AdoptedCandidate> {
    let worktree_path = options
        .source_worktree_path
        .as_deref()
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "source_worktree",
                "candidate adoption requires the recorded source worktree",
                None,
                None,
            )
        })?;
    let candidate = resolve_candidate(worktree_path, options.candidate_ref.as_deref())?;
    let (base_ref, historical_task_base, adoption_merge) = resolve_adoption_candidate_base(
        worktree_path,
        &candidate,
        options.task_base_sha.as_deref(),
        options.base_ref.as_deref(),
    )?;
    let changed_files = git_lines(
        worktree_path,
        &["diff", "--name-only", &base_ref, &candidate],
    )?;
    let commit_range = format!("{base_ref}..{candidate}");
    let commits = committed_change_evidence(worktree_path, &commit_range)?;
    if changed_files.is_empty() || commits.is_empty() {
        return Err(Error::validation_invalid_argument(
            "candidate_ref",
            "candidate revision contains no changes after the recorded task base",
            Some(candidate),
            None,
        ));
    }
    let tree_sha = git_stdout(
        worktree_path,
        &["rev-parse", &format!("{candidate}^{{tree}}")],
    )?
    .trim()
    .to_string();
    let adoption_merge = adoption_merge.map(|mut proof| {
        proof["candidate"] = json!(candidate);
        proof["changed_files"] = json!(changed_files);
        proof
    });
    Ok(AdoptedCandidate {
        provenance: json!({
            "change_source": "local_commits",
            "base_ref": base_ref,
            "commit_range": commit_range,
            "commits": commits,
            "historical_task_base": historical_task_base,
            "adoption_merge": adoption_merge,
        }),
        candidate: crate::agent_task::AgentTaskCandidate {
            base_sha: base_ref,
            candidate_sha: candidate,
            tree_sha,
        },
    })
}

/// Explicit adoption is scoped to the immutable candidate delta, not the task's
/// historical workspace base. A candidate may be a normal one-parent commit or
/// a two-parent merge that incorporates the verified base after it advanced.
fn resolve_adoption_candidate_base(
    cwd: &Path,
    candidate: &str,
    task_base_sha: Option<&str>,
    resolved_base_ref: Option<&str>,
) -> Result<(String, Option<String>, Option<Value>)> {
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
            let resolved_base = resolved_base_ref
                .filter(|base| !base.trim().is_empty())
                .map(|base| resolve_adoption_base_ref(cwd, base))
                .transpose()?
                .map(|base| base.trim().to_string())
                .ok_or_else(|| {
                    adoption_graph_error(
                        candidate,
                        "merge candidate requires the resolved immutable base ref",
                    )
                })?;
            if resolved_base != *resolved_base_parent {
                return Err(adoption_graph_error(
                    candidate,
                    "merge candidate's second parent does not equal the resolved immutable base",
                ));
            }
            let historical = historical_task_base.as_deref().ok_or_else(|| {
                adoption_graph_error(
                    candidate,
                    "merge candidate requires the recorded immutable task base",
                )
            })?;
            for (role, revision) in [
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
                resolved_base_parent.clone(),
                historical_task_base,
                Some(json!({
                    "candidate_parent": candidate_parent,
                    "resolved_base_parent": resolved_base_parent,
                    "candidate_tree": git_stdout(cwd, &["rev-parse", &format!("{candidate}^{{tree}}")])?.trim(),
                    "resolved_base_tree": git_stdout(
                        cwd,
                        &["rev-parse", &format!("{resolved_base_parent}^{{tree}}")],
                    )?
                    .trim(),
                })),
            ))
        }
        _ => Err(adoption_graph_error(
            candidate,
            "adopted candidate must have one parent or exactly two authenticated merge parents",
        )),
    }
}

/// Adoption freshness is resolved from the authoritative remote-tracking ref.
/// A local branch can lag (or point at an operator's unrelated checkout), while
/// the documented recovery merge is made from the exact `origin/<base>` commit.
/// An explicit object id remains authoritative and is never redirected.
fn resolve_adoption_base_ref(cwd: &Path, requested: &str) -> Result<String> {
    let requested = requested.trim();
    let mut candidates = Vec::new();
    if !requested.contains('/') && !is_full_git_object_id(requested) {
        candidates.push(format!("origin/{requested}"));
    }
    candidates.push(requested.to_string());
    candidates
        .into_iter()
        .find_map(|candidate| {
            git_stdout(
                cwd,
                &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
            )
            .ok()
        })
        .map(|base| base.trim().to_string())
        .ok_or_else(|| {
            adoption_graph_error(requested, "resolved immutable base ref is unavailable")
        })
}

fn is_full_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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

pub(super) fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
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

pub(super) fn git_lines(cwd: &Path, args: &[&str]) -> Result<Vec<String>> {
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
