//! Fresh provider attempts start from the Cook base, never from a pre-patched
//! destination (#16292).
//!
//! A recovery step that reserves a new provider attempt (`retry`, `fork
//! replacement`) used to hand the agent the destination exactly as the
//! previous attempt left it: the candidate commit or its uncommitted diff
//! already applied. The agent then saw its fix "already on the starting
//! commit" and declared the task incomplete. Before such a step runs, the
//! destination is snapshotted to a durable ref and reset to the recorded Cook
//! base, and [`assert_attempt_workspace_at_base`] refuses to start the attempt
//! with a typed [`AttemptWorkspaceNotAtBase`] failure otherwise.

use std::path::{Path, PathBuf};

use homeboy_core::{Error, Result};
use serde::Serialize;
use serde_json::Value;

use crate::agent_task_lifecycle;

/// Typed failure: an attempt would start from a workspace whose HEAD is not
/// the recorded Cook base or whose tree is not clean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptWorkspaceNotAtBase {
    pub workspace: PathBuf,
    pub base: String,
    pub head: String,
    pub dirty_paths: Vec<String>,
}

impl AttemptWorkspaceNotAtBase {
    pub const CODE: &'static str = "attempt_workspace_not_at_base";
}

impl std::fmt::Display for AttemptWorkspaceNotAtBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cook attempt workspace {} is not at its recorded base {} (HEAD {}, {} dirty path(s)); refusing to start a provider attempt from a pre-patched tree",
            self.workspace.display(),
            self.base,
            self.head,
            self.dirty_paths.len()
        )
    }
}

impl std::error::Error for AttemptWorkspaceNotAtBase {}

impl From<AttemptWorkspaceNotAtBase> for Error {
    fn from(failure: AttemptWorkspaceNotAtBase) -> Self {
        let mut error = Error::validation_invalid_argument(
            "attempt_workspace",
            failure.to_string(),
            Some(failure.workspace.display().to_string()),
            None,
        );
        error.details["failure"] = Value::String(AttemptWorkspaceNotAtBase::CODE.into());
        error.details["attempt_workspace"] = serde_json::to_value(&failure).unwrap_or(Value::Null);
        error.with_source(failure).with_retryable(false)
    }
}

fn git(workspace: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(workspace).args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "attempt_workspace",
            format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(workspace.display().to_string()),
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn head_and_dirty_paths(workspace: &Path) -> Result<(String, Vec<String>)> {
    let head = git(workspace, &["rev-parse", "--verify", "HEAD^{commit}"], None)?;
    let status = git(
        workspace,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        None,
    )?;
    let dirty = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .map(|entry| entry[3..].to_string())
        .collect();
    Ok((head, dirty))
}

/// Refuse to start an attempt unless the workspace HEAD is exactly `base` and
/// the tree is clean (ignored files excepted).
pub(crate) fn assert_attempt_workspace_at_base(workspace: &Path, base: &str) -> Result<()> {
    let base_sha = git(
        workspace,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        None,
    )?;
    let (head, dirty_paths) = head_and_dirty_paths(workspace)?;
    if head == base_sha && dirty_paths.is_empty() {
        return Ok(());
    }
    Err(AttemptWorkspaceNotAtBase {
        workspace: workspace.to_path_buf(),
        base: base_sha,
        head,
        dirty_paths,
    }
    .into())
}

/// Snapshot whatever the previous attempt left in `workspace` (commits ahead
/// of base and uncommitted, including untracked, changes) to
/// `refs/homeboy/attempt-reset/<label>`, reset the workspace to `base`, and
/// assert the attempt starts from that clean base. The snapshot uses a
/// private index, so neither the shared stash stack nor the destination index
/// is touched before the reset. Returns the snapshot evidence, or `None` when
/// the workspace was already at a clean base.
pub(crate) fn reset_attempt_workspace_to_base(
    workspace: &Path,
    base: &str,
    label: &str,
) -> Result<Option<Value>> {
    let base_sha = git(
        workspace,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        None,
    )?;
    let (head, dirty_paths) = head_and_dirty_paths(workspace)?;
    if head == base_sha && dirty_paths.is_empty() {
        return Ok(None);
    }
    let reference = format!("refs/homeboy/attempt-reset/{label}");
    git(workspace, &["check-ref-format", &reference], None)?;
    let index_file = homeboy_core::git::private_index(workspace)?;
    let index = index_file.path().to_path_buf();
    git(workspace, &["read-tree", "HEAD"], Some(&index))?;
    git(workspace, &["add", "--all"], Some(&index))?;
    let tree = git(workspace, &["write-tree"], Some(&index))?;
    let message = format!("homeboy: snapshot before fresh Cook attempt ({label})");
    let snapshot = git(
        workspace,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-m",
            &message,
        ],
        None,
    )?;
    git(workspace, &["update-ref", &reference, &snapshot], None)?;
    git(workspace, &["reset", "--hard", &base_sha], None)?;
    git(workspace, &["clean", "-fd"], None)?;
    assert_attempt_workspace_at_base(workspace, &base_sha)?;
    Ok(Some(serde_json::json!({
        "schema": "homeboy/cook-attempt-workspace-reset/v1",
        "workspace": workspace,
        "base": base_sha,
        "previous_head": head,
        "dirty_paths": dirty_paths,
        "snapshot_ref": reference,
        "snapshot": snapshot,
    })))
}

/// The recorded Cook base: the destination's starting HEAD the Cook diffs
/// against, else the first attempt's pinned workspace base.
pub(crate) fn recorded_cook_base(recipe: &super::AgentTaskCookRecipe) -> Option<String> {
    recipe
        .finalization
        .get("task_base_sha")
        .and_then(Value::as_str)
        .filter(|base| !base.trim().is_empty())
        .or_else(|| {
            recipe
                .attempts
                .first()
                .and_then(|attempt| attempt.plan.metadata.pointer("/cook_workspace_base/sha"))
                .and_then(Value::as_str)
                .filter(|base| !base.trim().is_empty())
        })
        .map(|base| base.trim().to_string())
}

/// The Homeboy-owned destination worktree of a Cook. Caller-owned `--cwd`
/// checkouts are never reset, so they resolve to `None`.
fn owned_attempt_workspace(recipe: &super::AgentTaskCookRecipe) -> Option<PathBuf> {
    let explicit_cwd = recipe
        .finalization
        .get("source_worktree_path")
        .is_some_and(|path| !path.is_null())
        || recipe.attempts.iter().any(|attempt| {
            attempt.plan.tasks.iter().any(|task| {
                task.metadata
                    .pointer("/worktree_provision/kind")
                    .and_then(Value::as_str)
                    == Some("explicit_cwd")
            })
        });
    if explicit_cwd {
        return None;
    }
    let handle = recipe.finalization.get("to_worktree")?.as_str()?;
    if Path::new(handle).is_dir() {
        return Some(PathBuf::from(handle));
    }
    homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(handle)
        .ok()
        .flatten()
        .map(|target| target.path)
        .filter(|path| path.is_dir())
}

/// Before a recovery step reserves a fresh provider attempt for `cook_id`,
/// reset its owned destination to the Cook base (snapshotting first) and
/// assert the attempt starts clean. Cooks without an owned local destination
/// or a recorded base are left to the attempt's own workspace admission.
pub(crate) fn prepare_fresh_attempt_workspace_for_cook(cook_id: &str, run_id: &str) -> Result<()> {
    let Ok(recipe) = super::load_recipe(cook_id) else {
        return Ok(());
    };
    let (Some(workspace), Some(base)) = (
        owned_attempt_workspace(&recipe),
        recorded_cook_base(&recipe),
    ) else {
        return Ok(());
    };
    crate::agent_task_promotion::destination::check(&workspace, Some(cook_id))?;
    let label = run_id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
    if let Some(evidence) = reset_attempt_workspace_to_base(&workspace, &base, &label)? {
        if let Ok(store) = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() {
            let _ = store.mutate_record(run_id, |record| {
                record.metadata["fresh_attempt_workspace_reset"] = evidence.clone();
                true
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, impl Fn(&[&str]) -> String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let run = move |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&path)
                .output()
                .expect("git");
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        (dir, run)
    }

    #[test]
    fn cook_fresh_attempt_starts_from_a_clean_base_tree_even_with_the_previous_diff_present() {
        let (dir, git) = repo();
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("lib.rs"), "fn buggy() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        // Attempt 1 left its candidate commit plus uncommitted work behind.
        std::fs::write(dir.path().join("lib.rs"), "fn fixed() {}\n").unwrap();
        git(&["commit", "-q", "-am", "homeboy: cook candidate"]);
        let candidate = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("regression_test.rs"), "#[test] fn t() {}\n").unwrap();

        let error = assert_attempt_workspace_at_base(dir.path(), &base).unwrap_err();
        assert_eq!(error.details["failure"], AttemptWorkspaceNotAtBase::CODE);
        assert_eq!(error.details["attempt_workspace"]["head"], candidate);

        let evidence = reset_attempt_workspace_to_base(dir.path(), &base, "cook-1-attempt-2")
            .unwrap()
            .expect("a pre-patched workspace is reset");
        assert_attempt_workspace_at_base(dir.path(), &base).unwrap();
        assert_eq!(git(&["rev-parse", "HEAD"]), base);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            "fn buggy() {}\n"
        );
        assert!(!dir.path().join("regression_test.rs").exists());
        // The previous attempt's work is preserved, not discarded.
        let snapshot = evidence["snapshot"].as_str().unwrap();
        assert_eq!(
            git(&["rev-parse", "refs/homeboy/attempt-reset/cook-1-attempt-2"]),
            snapshot
        );
        assert_eq!(git(&["rev-parse", &format!("{snapshot}^")]), candidate);
        assert_eq!(
            git(&["show", &format!("{snapshot}:regression_test.rs")]),
            "#[test] fn t() {}"
        );
        assert!(
            reset_attempt_workspace_to_base(dir.path(), &base, "cook-1-attempt-3")
                .unwrap()
                .is_none(),
            "a clean base tree is left alone"
        );
    }
}
