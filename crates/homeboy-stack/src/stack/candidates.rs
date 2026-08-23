//! Read-only discovery of installed stack alternatives.

use serde::Serialize;

use super::spec::{list, GitRef, StackPrEntry, StackProvenance, StackRequirements, StackSpec};
use super::status::{count_revs, git_ref_exists};
use homeboy_core::error::Result;
use std::path::Path;

/// A compatible installed stack that requires explicit operator selection.
#[derive(Debug, Clone, Serialize)]
pub struct StackCandidate {
    pub stack_id: String,
    pub component: String,
    pub repositories: Vec<String>,
    pub base: GitRef,
    pub target: GitRef,
    pub provenance: Option<StackProvenance>,
    pub requirements: StackRequirements,
    pub base_compatible: bool,
    pub pr_overlap: Vec<StackPrEntry>,
    pub selection_command: String,
}

/// Read-only evidence collected before a target branch can be recreated.
#[derive(Debug, Clone, Serialize)]
pub struct StackPreflight {
    pub target_exists: bool,
    pub target_ahead: Option<usize>,
    pub target_behind: Option<usize>,
    pub candidates: Vec<StackCandidate>,
    pub blocked: bool,
}

/// Discover alternatives for the same component and at least one repository.
///
/// Ranking is deterministic: declared base compatibility first, then the
/// number of overlapping PR coordinates, and finally stack ID.
pub fn discover(config_root: &Path, spec: &StackSpec) -> Result<Vec<StackCandidate>> {
    let source_repositories = repositories(&spec.prs);
    let mut candidates = list(config_root)?
        .into_iter()
        .filter(|candidate| candidate.id != spec.id && candidate.component == spec.component)
        .filter_map(|candidate| {
            let candidate_repositories = repositories(&candidate.prs);
            if source_repositories
                .iter()
                .all(|repo| !candidate_repositories.contains(repo))
            {
                return None;
            }

            let mut pr_overlap = candidate
                .prs
                .iter()
                .filter(|pr| {
                    spec.prs
                        .iter()
                        .any(|source| source.repo == pr.repo && source.number == pr.number)
                })
                .cloned()
                .collect::<Vec<_>>();
            pr_overlap.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
            let base_compatible = candidate.base == spec.base
                || candidate
                    .requirements
                    .compatible_bases
                    .iter()
                    .any(|base| base == &spec.base);

            Some(StackCandidate {
                stack_id: candidate.id.clone(),
                component: candidate.component,
                repositories: candidate_repositories,
                base: candidate.base,
                target: candidate.target,
                provenance: candidate.provenance,
                requirements: candidate.requirements,
                base_compatible,
                pr_overlap,
                selection_command: format!("homeboy stack apply {}", candidate.id),
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.base_compatible
            .cmp(&a.base_compatible)
            .then_with(|| b.pr_overlap.len().cmp(&a.pr_overlap.len()))
            .then_with(|| a.stack_id.cmp(&b.stack_id))
    });
    Ok(candidates)
}

/// Detect an installed alternative before a stale target is overwritten.
///
/// A target merely being behind its base is normal for an ordinary rebuild.
/// We stop only when a distinct-base candidate explicitly declares that it is
/// compatible with this stack's base; that is actionable evidence that the
/// operator may be using an obsolete stack definition.
pub fn preflight(
    config_root: &Path,
    spec: &StackSpec,
    path: &str,
    base_ref: &str,
) -> Result<StackPreflight> {
    let target_exists = git_ref_exists(path, &spec.target.branch);
    let (target_ahead, target_behind) = if target_exists {
        (
            count_revs(path, base_ref, &spec.target.branch),
            count_revs(path, &spec.target.branch, base_ref),
        )
    } else {
        (None, None)
    };
    let candidates = discover(config_root, spec)?;
    let blocked = target_behind.unwrap_or_default() > 0
        && candidates
            .iter()
            .any(|candidate| candidate.base_compatible && candidate.base != spec.base);

    Ok(StackPreflight {
        target_exists,
        target_ahead,
        target_behind,
        candidates,
        blocked,
    })
}

/// Build an error that preserves machine-readable alternatives for an
/// operator or client to select explicitly.
pub fn stale_stack_error(spec: &StackSpec, preflight: &StackPreflight) -> homeboy_core::Error {
    let mut error = homeboy_core::Error::git_command_failed(format!(
        "stack '{}' target '{}' is {} commit(s) behind '{}' and has explicitly compatible alternatives; refusing to recreate the target without an explicit stack selection",
        spec.id,
        spec.target.branch,
        preflight.target_behind.unwrap_or_default(),
        spec.base.display(),
    ));
    error.details["preflight"] = serde_json::to_value(preflight).unwrap_or_default();
    error.details["candidates"] = serde_json::to_value(&preflight.candidates).unwrap_or_default();
    error
}

fn repositories(prs: &[StackPrEntry]) -> Vec<String> {
    let mut repositories = prs.iter().map(|pr| pr.repo.clone()).collect::<Vec<_>>();
    repositories.sort();
    repositories.dedup();
    repositories
}

#[cfg(test)]
#[path = "../../../../tests/core/stack/candidates_test.rs"]
mod candidates_test;
