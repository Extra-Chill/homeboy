//! Git pipeline step plus post-op unmerged-index guard.

use super::super::expand::expand_vars;
use super::super::spec::{GitOp, RigSpec};
use super::component::resolve_component_path;
use crate::core::error::{Error, Result};

pub(super) fn run_git_step(
    rig: &RigSpec,
    component_id: &str,
    op: GitOp,
    extra_args: &[String],
) -> Result<()> {
    let (_, path) = resolve_component_path(rig, component_id)?;

    let base_args: Vec<String> = match op {
        GitOp::Status => vec!["status".into(), "--porcelain=v1".into()],
        GitOp::Pull => vec!["pull".into()],
        GitOp::Push => vec!["push".into()],
        GitOp::Fetch => vec!["fetch".into()],
        GitOp::Checkout => vec!["checkout".into()],
        GitOp::CurrentBranch => vec!["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
        GitOp::Rebase => vec!["rebase".into()],
        GitOp::CherryPick => vec!["cherry-pick".into()],
    };
    let mut full_args: Vec<String> = base_args;
    for arg in extra_args {
        full_args.push(expand_vars(rig, arg));
    }
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();

    let output = crate::core::git::execute_git_for_release(&path, &arg_refs).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!("spawn `git {}` in {}: {}", full_args.join(" "), path, e),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "`git {}` in {} exited {}{}",
                full_args.join(" "),
                path,
                code,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ),
        ));
    }
    fail_if_git_index_unmerged(rig, &path, &full_args)?;
    Ok(())
}

fn fail_if_git_index_unmerged(rig: &RigSpec, path: &str, completed_args: &[String]) -> Result<()> {
    let output = crate::core::git::execute_git_for_release(
        path,
        &["diff", "--name-only", "--diff-filter=U"],
    )
    .map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "spawn `git diff --name-only --diff-filter=U` in {}: {}",
                path, e
            ),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "`git diff --name-only --diff-filter=U` in {} exited {}{}",
                path,
                code,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ),
        ));
    }

    let unresolved = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if unresolved.is_empty() {
        return Ok(());
    }

    Err(Error::rig_pipeline_failed(
        &rig.id,
        "git",
        format!(
            "`git {}` in {} completed but left unresolved conflicts: {}",
            completed_args.join(" "),
            path,
            unresolved.join(", ")
        ),
    ))
}
