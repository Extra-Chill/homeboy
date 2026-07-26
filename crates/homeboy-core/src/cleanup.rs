use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use homeboy_extension_contract::ArtifactCleanupRetention;
use serde::{Deserialize, Serialize};

use crate::defaults::HomeboyConfig;
use crate::resource_cleanup_intent::ResourceCleanupIntent;
use crate::worktree_providers::{
    cleanup_worktree_providers_from_config, WorktreeProviderCleanupOptions,
    WorktreeProviderCleanupOutput,
};
use crate::{git, Error, Result};

mod cargo_targets;
pub use cargo_targets::{
    acquire_shared_cargo_target, cleanup_shared_cargo_targets, shared_cargo_target_inventory,
    CargoTargetCleanupOptions, CargoTargetCleanupOutput, SharedCargoTargetLease,
};
mod self_artifacts;

#[cfg(test)]
use self_artifacts::validate_homeboy_manifest_dir;
use self_artifacts::{homeboy_source_checkout, self_temp_artifact_candidates};

const ARTIFACT_DIR_REMOVE_ATTEMPTS: usize = 3;
const ARTIFACT_DIR_REMOVE_RETRY_DELAY: Duration = Duration::from_millis(50);
const BUILTIN_ARTIFACT_PATHS: &[(&str, &str)] = &[("target", "rust_target")];
const DEFAULT_EXTENSION_ARTIFACT_MIN_AGE_DAYS: u64 = 7;

#[derive(Debug, Clone, Default)]
pub struct ArtifactCleanupOptions {
    pub path: Option<PathBuf>,
    pub apply: bool,
    pub self_artifacts: bool,
    pub temp_roots: Vec<PathBuf>,
    pub sort: ArtifactCleanupSort,
    pub limit: Option<usize>,
    /// Minimum age for extension-declared artifacts. Core supplies the
    /// conservative default; extensions cannot weaken this policy.
    pub older_than_days: Option<u64>,
    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream (ancestor or patch-equivalent / squash-merged). This
    /// keeps in-progress cooks' build dirs intact while reclaiming the large
    /// `target/` dirs left behind by merged worktrees.
    pub merged_only: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArtifactCleanupSort {
    #[default]
    Discovery,
    Size,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceCleanupOptions {
    pub intent: ResourceCleanupIntent,
    pub artifacts: Option<ArtifactCleanupOptions>,
    pub worktree_providers: Option<WorktreeProviderCleanupOptions>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ResourceCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub skipped_count: usize,
    pub remaining_count: usize,
    pub reclaimed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<ArtifactCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_providers: Option<WorktreeProviderCleanupOutput>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub root: String,
    pub worktree_count: usize,
    pub candidate_count: usize,
    pub skipped_count: usize,
    pub applied_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub remaining_count: usize,
    pub estimated_bytes: u64,
    pub estimated_allocated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub reclaimed_allocated_bytes: u64,
    /// Replays the reviewed cleanup scope with mutation explicitly enabled.
    pub next_command: String,
    pub summary: ArtifactCleanupSummary,
    pub candidates: Vec<ArtifactCleanupCandidate>,
    pub skipped: Vec<ArtifactCleanupSkipped>,
    pub applied: Vec<ArtifactCleanupApplied>,
    pub failed: Vec<ArtifactCleanupFailed>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupSummary {
    pub invocation_reclaimed_bytes: u64,
    pub remaining_candidate_count: usize,
    pub remaining_candidate_bytes: u64,
    pub previous_session_reclaimed_bytes: u64,
    pub cumulative_session_reclaimed_bytes: u64,
    pub session_state_path: Option<String>,
    pub session_state_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ArtifactCleanupSessionState {
    cumulative_reclaimed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupCandidate {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub declared_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rehydrate: Option<String>,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    pub liveness: String,
    pub source_dirty: bool,
    pub unpushed_commits: bool,
    #[serde(skip)]
    extension_owned: bool,
    #[serde(skip)]
    minimum_age_days: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupSkipped {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rehydrate: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupApplied {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupFailed {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeInfo {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDeclaration {
    pub relative_path: String,
    pub kind: String,
    pub declared_by: String,
    pub category: Option<String>,
    pub rehydrate: Option<String>,
    pub retention: ArtifactCleanupRetention,
    pub extension_owned: bool,
}

#[derive(Debug, Default)]
struct GitSafety {
    source_dirty: bool,
    unpushed_commits: bool,
    dirty_paths: Vec<String>,
    untracked_paths: Vec<String>,
}

pub fn cleanup_artifacts(options: ArtifactCleanupOptions) -> Result<ArtifactCleanupOutput> {
    let root = resolve_root(&options)?;
    let worktrees = discover_worktrees(&root)?;
    cleanup_artifacts_in_worktrees(root, worktrees, &options, true)
}

/// Remove declared rebuildable artifacts from one completed worktree without
/// inspecting sibling worktrees that may still be owned by active tasks.
pub fn cleanup_worktree_artifacts(worktree: &Path) -> Result<ArtifactCleanupOutput> {
    let root = git_root(worktree)?;
    let worktree = root.clone();
    cleanup_artifacts_in_worktrees(
        root,
        vec![WorktreeInfo { path: worktree }],
        &ArtifactCleanupOptions {
            apply: true,
            ..Default::default()
        },
        false,
    )
}

/// Candidates and skips discovered for a single worktree.
struct WorktreeCandidateScan {
    candidates: Vec<ArtifactCleanupCandidate>,
    skipped: Vec<ArtifactCleanupSkipped>,
}

/// Scan one worktree for artifact-cleanup candidates. Fallible git/inventory
/// operations are contained here so the caller can skip a single bad worktree
/// (stale, non-Git, or vanished) without aborting the whole batch (#9925).
fn collect_worktree_candidates(
    worktree: &WorktreeInfo,
    options: &ArtifactCleanupOptions,
    active_worktree: Option<&Path>,
) -> Result<WorktreeCandidateScan> {
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    let safety = git_safety(&worktree.path)?;
    let declarations = artifact_declarations(&worktree.path)?;
    if options.merged_only && !branch_is_merged(&worktree.path) {
        for declaration in declarations {
            let artifact_path = worktree.path.join(&declaration.relative_path);
            if !artifact_path.exists() {
                continue;
            }
            skipped.push(skip_row(
                worktree,
                &declaration,
                artifact_path.to_string_lossy().to_string(),
                "worktree branch is not merged into its upstream",
            ));
        }
        return Ok(WorktreeCandidateScan {
            candidates,
            skipped,
        });
    }
    for declaration in declarations {
        let artifact_path = worktree.path.join(&declaration.relative_path);
        let display_path = artifact_path.to_string_lossy().to_string();
        if !artifact_path.exists() {
            continue;
        }
        if declaration.retention == ArtifactCleanupRetention::ReleaseAsset {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "extension declaration retains this release/package asset",
            ));
            continue;
        }
        if !is_safe_artifact_path(&declaration.relative_path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "declared artifact path is not a safe repo-relative path",
            ));
            continue;
        }
        if !artifact_is_contained(&worktree.path, &artifact_path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "artifact path resolves outside the worktree",
            ));
            continue;
        }
        if declaration.extension_owned
            && active_worktree.is_some_and(|active| same_path(active, &worktree.path))
        {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "active worktree is protected",
            ));
            continue;
        }
        if declaration.extension_owned && !branch_is_merged(&worktree.path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "extension artifact worktree branch is not merged into its upstream",
            ));
            continue;
        }
        if has_tracked_changes_under(&safety.dirty_paths, &declaration.relative_path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "artifact path contains tracked or staged source changes",
            ));
            continue;
        }
        if declaration.extension_owned
            && has_tracked_changes_under(&safety.untracked_paths, &declaration.relative_path)
        {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "artifact path contains untracked work that is not ignored",
            ));
            continue;
        }

        let age_seconds = path_age_seconds(&artifact_path);
        let minimum_age_days = options
            .older_than_days
            .unwrap_or(DEFAULT_EXTENSION_ARTIFACT_MIN_AGE_DAYS);
        if declaration.extension_owned
            && age_seconds.is_none_or(|age| age < days_to_seconds(minimum_age_days))
        {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                &format!("extension artifact is newer than {minimum_age_days} day age gate"),
            ));
            continue;
        }

        let storage = path_storage_measure(&artifact_path)?;
        candidates.push(ArtifactCleanupCandidate {
            worktree: worktree.path.to_string_lossy().to_string(),
            path: display_path.clone(),
            relative_path: declaration.relative_path.clone(),
            kind: declaration.kind.clone(),
            declared_by: declaration.declared_by.clone(),
            category: declaration.category.clone(),
            rehydrate: declaration.rehydrate.clone(),
            size_bytes: storage.logical_bytes,
            allocated_bytes: storage.allocated_bytes,
            age_seconds,
            liveness: if declaration.extension_owned {
                "merged_inactive".to_string()
            } else {
                "not_required".to_string()
            },
            source_dirty: safety.source_dirty,
            unpushed_commits: safety.unpushed_commits,
            extension_owned: declaration.extension_owned,
            minimum_age_days,
        });
    }

    Ok(WorktreeCandidateScan {
        candidates,
        skipped,
    })
}

/// A worktree-level skip row (no specific artifact declaration), used when an
/// entire worktree cannot be inspected.
fn worktree_skip_row(worktree: &WorktreeInfo, reason: String) -> ArtifactCleanupSkipped {
    ArtifactCleanupSkipped {
        worktree: worktree.path.to_string_lossy().to_string(),
        path: worktree.path.to_string_lossy().to_string(),
        relative_path: String::new(),
        kind: String::new(),
        declared_by: None,
        category: None,
        rehydrate: None,
        reason,
    }
}

fn cleanup_artifacts_in_worktrees(
    root: PathBuf,
    worktrees: Vec<WorktreeInfo>,
    options: &ArtifactCleanupOptions,
    include_self_temp_artifacts: bool,
) -> Result<ArtifactCleanupOutput> {
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    let active_worktree = std::env::current_dir()
        .ok()
        .and_then(|cwd| git_root(&cwd).ok());

    for worktree in &worktrees {
        // A single stale/non-Git/vanished worktree candidate must not abort the
        // whole batch: classify it, record a bounded diagnostic, and continue so
        // independent valid worktrees are still cleaned (#9925).
        match collect_worktree_candidates(worktree, options, active_worktree.as_deref()) {
            Ok(WorktreeCandidateScan {
                candidates: worktree_candidates,
                skipped: worktree_skipped,
            }) => {
                candidates.extend(worktree_candidates);
                skipped.extend(worktree_skipped);
            }
            Err(error) => {
                skipped.push(worktree_skip_row(
                    worktree,
                    format!("worktree could not be inspected and was skipped: {error}"),
                ));
            }
        }
    }

    if include_self_temp_artifacts {
        for candidate in self_temp_artifact_candidates(options)? {
            candidates.push(candidate);
        }
    }

    order_and_limit_candidates(&mut candidates, options.sort, options.limit);

    let (applied, failed) = if options.apply {
        apply_artifact_candidates(&candidates, |candidate| {
            let path = Path::new(&candidate.path);
            path.exists().then(|| {
                revalidate_artifact_candidate(candidate, active_worktree.as_deref())?;
                remove_artifact_path(path)
            })
        })
    } else {
        (Vec::new(), Vec::new())
    };

    let estimated_bytes = candidates.iter().map(|row| row.size_bytes).sum();
    let estimated_allocated_bytes = candidates.iter().map(|row| row.allocated_bytes).sum();
    let reclaimed_bytes = applied.iter().map(|row| row.size_bytes).sum();
    let reclaimed_allocated_bytes = applied.iter().map(|row| row.allocated_bytes).sum();
    let (success_count, remaining_count) =
        artifact_cleanup_result_counts(candidates.len(), applied.len(), failed.len());
    let failure_count = failed.len();
    let (_, remaining_candidate_bytes) = remaining_candidate_totals(&candidates, options.apply);
    let summary = cleanup_summary(
        &root,
        options.apply,
        reclaimed_bytes,
        remaining_count,
        remaining_candidate_bytes,
    );

    Ok(ArtifactCleanupOutput {
        command: "cleanup.artifacts",
        mode: if options.apply { "apply" } else { "dry_run" },
        root: root.to_string_lossy().to_string(),
        worktree_count: worktrees.len(),
        candidate_count: candidates.len(),
        skipped_count: skipped.len(),
        applied_count: success_count,
        success_count,
        failure_count,
        remaining_count,
        estimated_bytes,
        estimated_allocated_bytes,
        reclaimed_bytes,
        reclaimed_allocated_bytes,
        next_command: artifact_cleanup_apply_command(options),
        summary,
        candidates,
        skipped,
        applied,
        failed,
    })
}

fn artifact_cleanup_apply_command(options: &ArtifactCleanupOptions) -> String {
    use crate::engine::shell::quote_arg;

    let mut command = "homeboy cleanup artifacts".to_string();
    if options.self_artifacts {
        command.push_str(" --self");
    } else if let Some(path) = &options.path {
        command.push_str(&format!(" --path {}", quote_arg(&path.to_string_lossy())));
    }
    for temp_root in &options.temp_roots {
        command.push_str(&format!(
            " --temp-root {}",
            quote_arg(&temp_root.to_string_lossy())
        ));
    }
    if options.sort == ArtifactCleanupSort::Size {
        command.push_str(" --sort size");
    }
    if let Some(limit) = options.limit {
        command.push_str(&format!(" --limit {limit}"));
    }
    if let Some(days) = options.older_than_days {
        command.push_str(&format!(" --older-than-days {days}"));
    }
    if options.merged_only {
        command.push_str(" --merged-only");
    }
    command.push_str(" --apply");
    command
}

pub fn cleanup_resources_from_config(
    mut options: ResourceCleanupOptions,
    config: HomeboyConfig,
) -> Result<ResourceCleanupOutput> {
    let apply = options.intent.is_apply();
    let mut artifacts = None;
    let mut providers = None;

    if let Some(mut artifact_options) = options.artifacts.take() {
        artifact_options.apply = apply;
        artifacts = Some(cleanup_artifacts(artifact_options)?);
    }

    if let Some(mut provider_options) = options.worktree_providers.take() {
        provider_options.apply = apply;
        providers = Some(cleanup_worktree_providers_from_config(
            provider_options,
            config,
        )?);
    }

    let candidate_count = artifacts
        .as_ref()
        .map(|output| output.candidate_count)
        .unwrap_or(0);
    let applied_count = artifacts
        .as_ref()
        .map(|output| output.applied_count)
        .unwrap_or(0);
    let artifact_success_count = artifacts
        .as_ref()
        .map(|output| output.success_count)
        .unwrap_or(0);
    let artifact_failure_count = artifacts
        .as_ref()
        .map(|output| output.failure_count)
        .unwrap_or(0);
    let skipped_count = artifacts
        .as_ref()
        .map(|output| output.skipped_count)
        .unwrap_or(0);
    let remaining_count = artifacts
        .as_ref()
        .map(|output| output.remaining_count)
        .unwrap_or(0);
    let reclaimed_bytes = artifacts
        .as_ref()
        .map(|output| output.reclaimed_bytes)
        .unwrap_or(0);
    let provider_success_count = providers
        .as_ref()
        .map(|output| output.success_count)
        .unwrap_or(0);
    let provider_failure_count = providers
        .as_ref()
        .map(|output| output.failure_count)
        .unwrap_or(0);

    let (success_count, failure_count) = if providers.is_some() {
        (provider_success_count, provider_failure_count)
    } else {
        (artifact_success_count, artifact_failure_count)
    };

    Ok(ResourceCleanupOutput {
        command: "cleanup.resources",
        mode: options.intent.as_str(),
        candidate_count,
        applied_count,
        success_count,
        failure_count,
        skipped_count,
        remaining_count,
        reclaimed_bytes,
        artifacts,
        worktree_providers: providers,
    })
}

fn order_and_limit_candidates(
    candidates: &mut Vec<ArtifactCleanupCandidate>,
    sort: ArtifactCleanupSort,
    limit: Option<usize>,
) {
    if sort == ArtifactCleanupSort::Size {
        candidates.sort_by(|left, right| {
            right
                .size_bytes
                .cmp(&left.size_bytes)
                .then_with(|| left.path.cmp(&right.path))
        });
    }

    if let Some(limit) = limit {
        candidates.truncate(limit);
    }
}

fn cleanup_summary(
    root: &Path,
    apply: bool,
    invocation_reclaimed_bytes: u64,
    remaining_candidate_count: usize,
    remaining_candidate_bytes: u64,
) -> ArtifactCleanupSummary {
    let mut session_state_path = None;
    let mut session_state_error = None;
    let mut previous_session_reclaimed_bytes = 0;
    let mut cumulative_session_reclaimed_bytes = invocation_reclaimed_bytes;

    match cleanup_session_state_path(root) {
        Ok(path) => {
            session_state_path = Some(path.to_string_lossy().to_string());
            let mut state = read_cleanup_session_state(&path);
            previous_session_reclaimed_bytes = state.cumulative_reclaimed_bytes;
            if apply {
                state.cumulative_reclaimed_bytes = state
                    .cumulative_reclaimed_bytes
                    .saturating_add(invocation_reclaimed_bytes);
                cumulative_session_reclaimed_bytes = state.cumulative_reclaimed_bytes;
                if let Err(error) = write_cleanup_session_state(&path, &state) {
                    session_state_error = Some(error);
                }
            } else {
                cumulative_session_reclaimed_bytes = state.cumulative_reclaimed_bytes;
            }
        }
        Err(error) => {
            session_state_error = Some(error.to_string());
        }
    }

    ArtifactCleanupSummary {
        invocation_reclaimed_bytes,
        remaining_candidate_count,
        remaining_candidate_bytes,
        previous_session_reclaimed_bytes,
        cumulative_session_reclaimed_bytes,
        session_state_path,
        session_state_error,
    }
}

fn cleanup_session_state_path(root: &Path) -> Result<PathBuf> {
    let output = git::run_git(root, &["rev-parse", "--git-common-dir"], "git common dir")?;
    let git_common_dir = PathBuf::from(output.trim());
    let git_common_dir = if git_common_dir.is_absolute() {
        git_common_dir
    } else {
        root.join(git_common_dir)
    };
    Ok(git_common_dir.join("homeboy-cleanup-artifacts-session.json"))
}

fn read_cleanup_session_state(path: &Path) -> ArtifactCleanupSessionState {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_cleanup_session_state(
    path: &Path,
    state: &ArtifactCleanupSessionState,
) -> std::result::Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let raw = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    fs::write(path, raw).map_err(|error| error.to_string())
}

fn remaining_candidate_totals(
    candidates: &[ArtifactCleanupCandidate],
    apply: bool,
) -> (usize, u64) {
    if !apply {
        return (
            candidates.len(),
            candidates.iter().map(|row| row.size_bytes).sum(),
        );
    }

    let mut count = 0;
    let mut bytes = 0;
    for candidate in candidates {
        let path = Path::new(&candidate.path);
        if path.exists() {
            count += 1;
            bytes += path_size(path).unwrap_or(candidate.size_bytes);
        }
    }
    (count, bytes)
}

/// Produces the artifact-cleanup result counters from per-candidate outcomes.
/// Skipped paths are filtered before candidacy, so each candidate is either
/// successfully applied or remains after the invocation; failed removals are
/// therefore a subset of remaining candidates.
fn artifact_cleanup_result_counts(
    candidate_count: usize,
    applied_count: usize,
    failure_count: usize,
) -> (usize, usize) {
    debug_assert!(applied_count <= candidate_count);
    let remaining_count = candidate_count - applied_count;
    debug_assert!(failure_count <= remaining_count);
    (applied_count, remaining_count)
}

fn apply_artifact_candidates<Remove>(
    candidates: &[ArtifactCleanupCandidate],
    mut remove: Remove,
) -> (Vec<ArtifactCleanupApplied>, Vec<ArtifactCleanupFailed>)
where
    Remove: FnMut(&ArtifactCleanupCandidate) -> Option<Result<()>>,
{
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for candidate in candidates {
        match remove(candidate) {
            Some(Ok(())) => applied.push(applied_row(candidate)),
            Some(Err(error)) => failed.push(failed_row(candidate, error.message)),
            None => {}
        }
    }
    (applied, failed)
}

fn resolve_root(options: &ArtifactCleanupOptions) -> Result<PathBuf> {
    if options.path.is_some() && options.self_artifacts {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            "cannot be combined with path",
            None,
            None,
        ));
    }

    let start = match options.path.as_deref() {
        Some(path) => path.to_path_buf(),
        None if options.self_artifacts => homeboy_source_checkout()?,
        None => std::env::current_dir().map_err(|e| {
            Error::internal_io(e.to_string(), Some("read current directory".to_string()))
        })?,
    };
    git_root(&start)
}

fn discover_worktrees(root: &Path) -> Result<Vec<WorktreeInfo>> {
    let output = git::run_git(
        root,
        &["worktree", "list", "--porcelain"],
        "git worktree list",
    )?;
    let mut worktrees = Vec::new();
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push(WorktreeInfo {
                path: PathBuf::from(path),
            });
        }
    }
    if worktrees.is_empty() {
        worktrees.push(WorktreeInfo {
            path: root.to_path_buf(),
        });
    }
    Ok(worktrees)
}

pub fn artifact_declarations(worktree: &Path) -> Result<Vec<ArtifactDeclaration>> {
    let mut declarations: Vec<ArtifactDeclaration> = BUILTIN_ARTIFACT_PATHS
        .iter()
        .map(|(relative_path, kind)| ArtifactDeclaration {
            relative_path: (*relative_path).to_string(),
            kind: (*kind).to_string(),
            declared_by: "homeboy:builtin_artifact_paths".to_string(),
            category: None,
            rehydrate: None,
            retention: ArtifactCleanupRetention::Reconstructable,
            extension_owned: false,
        })
        .collect();

    let config_path = worktree.join("homeboy.json");
    if config_path.exists() {
        let raw = fs::read_to_string(&config_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read {}", config_path.display())),
            )
        })?;
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some(format!("parse {}", config_path.display())),
            )
        })?;
        for path in value
            .get("artifact_cleanup_paths")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            declarations.push(ArtifactDeclaration {
                relative_path: path.to_string(),
                kind: "declared_artifact".to_string(),
                declared_by: "homeboy.json:artifact_cleanup_paths".to_string(),
                category: None,
                rehydrate: None,
                retention: ArtifactCleanupRetention::Reconstructable,
                extension_owned: false,
            });
        }
        declarations.extend(extension_artifact_declarations(worktree, &value)?);
    }

    declarations.sort_by_key(|row| match (row.extension_owned, row.retention) {
        (true, ArtifactCleanupRetention::ReleaseAsset) => 0,
        (true, ArtifactCleanupRetention::Reconstructable) => 1,
        (false, _) => 2,
    });
    let mut seen = HashSet::new();
    declarations.retain(|row| seen.insert(row.relative_path.clone()));
    Ok(declarations)
}

fn extension_artifact_declarations(
    worktree: &Path,
    portable: &serde_json::Value,
) -> Result<Vec<ArtifactDeclaration>> {
    let Some(extensions) = portable
        .get("extensions")
        .and_then(|value| value.as_object())
    else {
        return Ok(Vec::new());
    };
    let mut declarations = Vec::new();
    for extension_id in extensions.keys() {
        let Ok(manifest) = crate::extension_store::load_extension(extension_id) else {
            continue;
        };
        let Some(build) = manifest.build else {
            continue;
        };
        let rules = build.artifact_cleanup;
        let artifact_paths: Vec<PathBuf> = rules
            .iter()
            .filter(|rule| is_safe_artifact_path(&rule.path))
            .map(|rule| PathBuf::from(&rule.path))
            .collect();
        for rule in rules {
            if rule.category.trim().is_empty()
                || rule.rehydrate.trim().is_empty()
                || !is_safe_artifact_path(&rule.path)
            {
                continue;
            }
            let scopes = if rule.manifest_names.is_empty() {
                vec![worktree.to_path_buf()]
            } else {
                manifest_scopes(worktree, &rule.manifest_names, &artifact_paths)?
            };
            for scope in scopes {
                let path = scope.join(&rule.path);
                let Ok(relative) = path.strip_prefix(worktree) else {
                    continue;
                };
                let relative_path = relative.to_string_lossy().to_string();
                if !is_safe_artifact_path(&relative_path) {
                    continue;
                }
                declarations.push(ArtifactDeclaration {
                    relative_path,
                    kind: "extension_artifact".to_string(),
                    declared_by: format!("extension:{extension_id}"),
                    category: Some(rule.category.clone()),
                    rehydrate: Some(rule.rehydrate.clone()),
                    retention: rule.retention,
                    extension_owned: true,
                });
            }
        }
    }
    Ok(declarations)
}

fn manifest_scopes(
    worktree: &Path,
    manifest_names: &[String],
    artifact_paths: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let names: HashSet<&str> = manifest_names
        .iter()
        .filter_map(|name| {
            let path = Path::new(name);
            (path.components().count() == 1
                && matches!(
                    path.components().next(),
                    Some(std::path::Component::Normal(_))
                ))
            .then_some(name.as_str())
        })
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut scopes = Vec::new();
    collect_manifest_scopes(worktree, &names, artifact_paths, &mut scopes)?;
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}

fn collect_manifest_scopes(
    directory: &Path,
    manifest_names: &HashSet<&str>,
    artifact_paths: &[PathBuf],
    scopes: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read manifest scope {}", directory.display())),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read manifest scope entry {}", directory.display())),
            )
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
        })?;
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_file() && manifest_names.contains(name.as_ref()) {
            scopes.push(directory.to_path_buf());
        } else if file_type.is_dir()
            && name != ".git"
            && !artifact_paths
                .iter()
                .any(|artifact| path.ends_with(artifact))
        {
            collect_manifest_scopes(&path, manifest_names, artifact_paths, scopes)?;
        }
    }
    Ok(())
}

fn git_safety(worktree: &Path) -> Result<GitSafety> {
    let status = git::run_git(
        worktree,
        &[
            "status",
            "--porcelain=v1",
            "--ignored",
            "--untracked-files=normal",
        ],
        "git status",
    )?;
    let mut dirty_paths = Vec::new();
    let mut untracked_paths = Vec::new();
    let mut source_dirty = false;
    for line in status.lines() {
        if line.len() < 4 || line.starts_with("!! ") {
            continue;
        }
        let path = status_path(line);
        if line.starts_with("?? ") {
            if !path.is_empty() {
                untracked_paths.push(path);
            }
            continue;
        }
        if !path.is_empty() {
            source_dirty = true;
            dirty_paths.push(path);
        }
    }

    let unpushed_commits = match git::run_git(
        worktree,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    ) {
        Ok(count) => count.trim().parse::<u32>().unwrap_or(0) > 0,
        Err(_) => false,
    };

    Ok(GitSafety {
        source_dirty,
        unpushed_commits,
        dirty_paths,
        untracked_paths,
    })
}

/// Returns true when the worktree's current branch is already merged into its
/// upstream tracking branch. "Merged" covers three git-native cases, so it is
/// agnostic to merge strategy and ecosystem:
///   1. HEAD has no commits ahead of `@{upstream}` (fast-forward / ancestor).
///   2. Every commit ahead of `@{upstream}` is reported as already-applied by
///      `git cherry` (prefix `-`), i.e. patch-equivalent — the rebase merge.
///   3. Same patch-equivalence covers squash-merges whose single commit lands
///      upstream with a matching patch-id.
///
/// When upstream cannot be resolved (no tracking branch) the worktree is
/// treated as NOT merged, so its artifacts are preserved conservatively.
fn branch_is_merged(worktree: &Path) -> bool {
    let ahead = match git::run_git(
        worktree,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    ) {
        Ok(count) => count.trim().parse::<u32>().unwrap_or(u32::MAX),
        Err(_) => return false,
    };
    if ahead == 0 {
        return true;
    }

    // Commits exist ahead of upstream; treat as merged only if git reports
    // every one of them as already applied upstream (patch-equivalent).
    match git::run_git(
        worktree,
        &["cherry", "@{upstream}", "HEAD"],
        "git cherry upstream",
    ) {
        Ok(output) => {
            let mut saw_commit = false;
            for line in output.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                saw_commit = true;
                // `+ <sha>` means the commit is NOT present upstream.
                if line.starts_with('+') {
                    return false;
                }
            }
            saw_commit
        }
        Err(_) => false,
    }
}

fn status_path(line: &str) -> String {
    let raw = line.get(3..).unwrap_or_default();
    raw.rsplit(" -> ")
        .next()
        .unwrap_or(raw)
        .trim_matches('"')
        .to_string()
}

fn has_tracked_changes_under(dirty_paths: &[String], relative_path: &str) -> bool {
    let prefix = format!("{}/", relative_path.trim_end_matches('/'));
    dirty_paths
        .iter()
        .any(|path| path == relative_path || path.starts_with(&prefix))
}

fn applied_row(candidate: &ArtifactCleanupCandidate) -> ArtifactCleanupApplied {
    ArtifactCleanupApplied {
        worktree: candidate.worktree.clone(),
        path: candidate.path.clone(),
        relative_path: candidate.relative_path.clone(),
        kind: candidate.kind.clone(),
        size_bytes: candidate.size_bytes,
        allocated_bytes: candidate.allocated_bytes,
        removed: true,
    }
}

fn failed_row(candidate: &ArtifactCleanupCandidate, error: String) -> ArtifactCleanupFailed {
    ArtifactCleanupFailed {
        worktree: candidate.worktree.clone(),
        path: candidate.path.clone(),
        relative_path: candidate.relative_path.clone(),
        kind: candidate.kind.clone(),
        size_bytes: candidate.size_bytes,
        allocated_bytes: candidate.allocated_bytes,
        error,
    }
}

fn skip_row(
    worktree: &WorktreeInfo,
    declaration: &ArtifactDeclaration,
    path: String,
    reason: &str,
) -> ArtifactCleanupSkipped {
    ArtifactCleanupSkipped {
        worktree: worktree.path.to_string_lossy().to_string(),
        path,
        relative_path: declaration.relative_path.clone(),
        kind: declaration.kind.clone(),
        declared_by: Some(declaration.declared_by.clone()),
        category: declaration.category.clone(),
        rehydrate: declaration.rehydrate.clone(),
        reason: reason.to_string(),
    }
}

pub fn is_safe_artifact_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    !relative_path.is_empty()
        && relative_path != "."
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

#[derive(Debug, Clone, Copy)]
struct PathStorageMeasure {
    logical_bytes: u64,
    allocated_bytes: u64,
}

fn path_size(path: &Path) -> Result<u64> {
    Ok(path_storage_measure(path)?.logical_bytes)
}

fn path_storage_measure(path: &Path) -> Result<PathStorageMeasure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(PathStorageMeasure {
            logical_bytes: metadata.len(),
            allocated_bytes: allocated_bytes(&metadata),
        });
    }

    // Sum only the reclaimable file/symlink content. A directory's own
    // `metadata.len()` is the inode/directory-entry size (typically a 4 KiB
    // block on ext4/tmpfs), not reclaimable payload — counting it made size
    // sorting reflect directory nesting depth rather than actual artifact
    // weight (e.g. a 5-byte file under two nested dirs outranking a 256-byte
    // file under one). Recurse over children and count their bytes only.
    let mut total = PathStorageMeasure {
        logical_bytes: 0,
        allocated_bytes: allocated_bytes(&metadata),
    };
    for entry in fs::read_dir(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read directory {}", path.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read directory entry {}", path.display())),
            )
        })?;
        let child = path_storage_measure(&entry.path())?;
        total.logical_bytes = total.logical_bytes.saturating_add(child.logical_bytes);
        total.allocated_bytes = total.allocated_bytes.saturating_add(child.allocated_bytes);
    }
    Ok(total)
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

fn days_to_seconds(days: u64) -> u64 {
    days.saturating_mul(24 * 60 * 60)
}

fn path_age_seconds(path: &Path) -> Option<u64> {
    latest_modified(path)?
        .elapsed()
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

fn latest_modified(path: &Path) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path).ok()? {
            let modified = latest_modified(&entry.ok()?.path())?;
            latest = latest.max(modified);
        }
    }
    Some(latest)
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok() == right.canonicalize().ok()
}

fn artifact_is_contained(worktree: &Path, artifact: &Path) -> bool {
    let Ok(worktree) = worktree.canonicalize() else {
        return false;
    };
    let Ok(artifact) = artifact.canonicalize() else {
        return false;
    };
    artifact.starts_with(&worktree) && artifact != worktree
}

fn revalidate_artifact_candidate(
    candidate: &ArtifactCleanupCandidate,
    active_worktree: Option<&Path>,
) -> Result<()> {
    let worktree = Path::new(&candidate.worktree);
    let artifact = Path::new(&candidate.path);
    if !is_safe_artifact_path(&candidate.relative_path)
        || !artifact_is_contained(worktree, artifact)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_path",
            "artifact path failed containment revalidation",
            Some(candidate.path.clone()),
            None,
        ));
    }
    if !candidate.extension_owned {
        return Ok(());
    }
    if active_worktree.is_some_and(|active| same_path(active, worktree)) {
        return Err(Error::validation_invalid_argument(
            "artifact_path",
            "active worktree became ineligible before removal",
            Some(candidate.worktree.clone()),
            None,
        ));
    }
    if !branch_is_merged(worktree) {
        return Err(Error::validation_invalid_argument(
            "artifact_path",
            "worktree branch became unmerged before removal",
            Some(candidate.worktree.clone()),
            None,
        ));
    }
    let safety = git_safety(worktree)?;
    if has_tracked_changes_under(&safety.dirty_paths, &candidate.relative_path)
        || has_tracked_changes_under(&safety.untracked_paths, &candidate.relative_path)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_path",
            "artifact path became dirty before removal",
            Some(candidate.path.clone()),
            None,
        ));
    }
    if path_age_seconds(artifact)
        .is_none_or(|age| age < days_to_seconds(candidate.minimum_age_days))
    {
        return Err(Error::validation_invalid_argument(
            "artifact_path",
            "artifact path no longer satisfies its age gate",
            Some(candidate.path.clone()),
            None,
        ));
    }
    Ok(())
}

fn remove_artifact_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        remove_artifact_directory(path)
    } else {
        fs::remove_file(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("remove file {}", path.display())),
            )
        })
    }
}

fn remove_artifact_directory(path: &Path) -> Result<()> {
    remove_artifact_directory_with(path, |path| fs::remove_dir_all(path), std::thread::sleep)
}

fn remove_artifact_directory_with<Remove, Sleep>(
    path: &Path,
    mut remove_dir_all: Remove,
    mut sleep: Sleep,
) -> Result<()>
where
    Remove: FnMut(&Path) -> io::Result<()>,
    Sleep: FnMut(Duration),
{
    for attempt in 1..=ARTIFACT_DIR_REMOVE_ATTEMPTS {
        match remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error)
                if error.kind() == io::ErrorKind::DirectoryNotEmpty
                    && attempt < ARTIFACT_DIR_REMOVE_ATTEMPTS =>
            {
                sleep(ARTIFACT_DIR_REMOVE_RETRY_DELAY);
            }
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(format!("remove directory {}", path.display())),
                ));
            }
        }
    }

    Ok(())
}

fn git_root(path: &Path) -> Result<PathBuf> {
    let output = git::run_git(path, &["rev-parse", "--show-toplevel"], "git root").map_err(|_| {
        Error::validation_invalid_argument(
            "path",
            format!(
                "{} is not inside a git checkout; run `homeboy cleanup artifacts` from a checkout or pass `--path <PATH>`",
                path.display()
            ),
            Some(path.to_string_lossy().to_string()),
            None,
        )
        .with_hint(
            "Run from a git checkout or pass `--path <PATH>`, for example: `homeboy cleanup artifacts --path /path/to/checkout`.",
        )
    })?;
    Ok(PathBuf::from(output.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::process::Command;
    use tempfile::TempDir;

    use crate::defaults::{WorktreeProviderCommands, WorktreeProviderConfig, WorktreeProviderKind};

    #[test]
    fn safe_artifact_paths_are_repo_relative() {
        assert!(is_safe_artifact_path("target"));
        assert!(is_safe_artifact_path("runtime/generated-fixture"));
        assert!(!is_safe_artifact_path(""));
        assert!(!is_safe_artifact_path("."));
        assert!(!is_safe_artifact_path("./target"));
        assert!(!is_safe_artifact_path("../target"));
        assert!(!is_safe_artifact_path("/tmp/target"));
    }

    #[test]
    fn tracked_changes_under_artifact_path_are_detected() {
        let dirty = vec!["target/generated.rs".to_string(), "src/lib.rs".to_string()];
        assert!(has_tracked_changes_under(&dirty, "target"));
        assert!(!has_tracked_changes_under(&dirty, "node_modules"));
    }

    #[test]
    fn declared_artifact_paths_are_loaded_from_homeboy_json() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("homeboy.json"),
            r#"{"artifact_cleanup_paths":["runtime/generated-fixture","target"]}"#,
        )
        .expect("write config");

        let declarations = artifact_declarations(tmp.path()).expect("declarations");

        assert!(declarations
            .iter()
            .any(|row| row.relative_path == "runtime/generated-fixture"));
        assert_eq!(
            declarations
                .iter()
                .filter(|row| row.relative_path == "target")
                .count(),
            1,
            "declared paths should not duplicate builtins"
        );
        let target = declarations
            .iter()
            .find(|row| row.relative_path == "target")
            .expect("target declaration");
        assert_eq!(target.kind, "rust_target");
        assert_eq!(target.declared_by, "homeboy:builtin_artifact_paths");
    }

    #[test]
    fn artifact_declarations_include_builtin_rust_target() {
        let tmp = TempDir::new().expect("tempdir");

        let declarations = artifact_declarations(tmp.path()).expect("declarations");

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].relative_path, "target");
        assert_eq!(declarations[0].kind, "rust_target");
        assert_eq!(
            declarations[0].declared_by,
            "homeboy:builtin_artifact_paths"
        );
    }

    #[test]
    fn extension_declarations_resolve_only_exact_manifest_scopes_with_guidance() {
        crate::test_support::with_isolated_home(|_| {
            let repo = TempDir::new().expect("repo");
            install_cleanup_extension(serde_json::json!([
                {
                    "category": "dependencies",
                    "path": "deps",
                    "manifest_names": ["project.manifest"],
                    "rehydrate": "tool install"
                },
                {
                    "category": "package",
                    "path": "release",
                    "retention": "release_asset",
                    "rehydrate": "tool package"
                }
            ]));
            write_file(
                &repo.path().join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"fixture":{}}}"#,
            );
            write_file(&repo.path().join("project.manifest"), "root");
            write_file(&repo.path().join("packages/one/project.manifest"), "nested");
            write_file(&repo.path().join("deps/project.manifest"), "artifact-owned");
            write_file(
                &repo.path().join("packages/two/other.manifest"),
                "unsupported",
            );

            let declarations = artifact_declarations(repo.path()).expect("declarations");

            let extension_paths: Vec<&str> = declarations
                .iter()
                .filter(|row| row.extension_owned)
                .map(|row| row.relative_path.as_str())
                .collect();
            assert!(extension_paths.contains(&"deps"));
            assert!(extension_paths.contains(&"packages/one/deps"));
            assert!(!extension_paths.contains(&"deps/deps"));
            assert!(!extension_paths.contains(&"packages/two/deps"));
            let nested = declarations
                .iter()
                .find(|row| row.relative_path == "packages/one/deps")
                .expect("nested declaration");
            assert_eq!(nested.category.as_deref(), Some("dependencies"));
            assert_eq!(nested.rehydrate.as_deref(), Some("tool install"));
            assert_eq!(nested.declared_by, "extension:fixture");
            let release = declarations
                .iter()
                .find(|row| row.relative_path == "release")
                .expect("release declaration");
            assert_eq!(release.retention, ArtifactCleanupRetention::ReleaseAsset);
        });
    }

    #[test]
    fn extension_artifacts_require_inactive_merged_and_old_worktrees() {
        crate::test_support::with_isolated_home(|_| {
            install_cleanup_extension(serde_json::json!([{
                "category": "cache",
                "path": "cache",
                "rehydrate": "tool warm"
            }]));
            let (_remote, repo) = extension_git_repo();
            write_file(&repo.path().join(".gitignore"), "cache/\n");
            git(repo.path(), &["add", ".gitignore"]);
            git_commit(repo.path(), "ignore cache");
            git(repo.path(), &["push"]);
            write_file(&repo.path().join("cache/item"), "artifact");
            let worktree = WorktreeInfo {
                path: repo.path().to_path_buf(),
            };
            let options = ArtifactCleanupOptions {
                older_than_days: Some(0),
                ..Default::default()
            };

            let active = collect_worktree_candidates(&worktree, &options, Some(repo.path()))
                .expect("active scan");
            assert!(active
                .skipped
                .iter()
                .any(|row| row.reason.contains("active worktree")));

            write_file(&repo.path().join("src/feature.rs"), "feature");
            git(repo.path(), &["add", "src/feature.rs"]);
            git_commit(repo.path(), "unmerged feature");
            let unmerged =
                collect_worktree_candidates(&worktree, &options, None).expect("unmerged scan");
            assert!(unmerged
                .skipped
                .iter()
                .any(|row| row.reason.contains("not merged")));

            git(repo.path(), &["push"]);
            let recent = collect_worktree_candidates(
                &worktree,
                &ArtifactCleanupOptions {
                    older_than_days: Some(1),
                    ..Default::default()
                },
                None,
            )
            .expect("age scan");
            assert!(recent
                .skipped
                .iter()
                .any(|row| row.reason.contains("age gate")));
        });
    }

    #[test]
    fn extension_artifacts_preserve_untracked_staged_and_release_paths() {
        crate::test_support::with_isolated_home(|_| {
            install_cleanup_extension(serde_json::json!([
                {
                    "category": "cache",
                    "path": "cache",
                    "rehydrate": "tool warm"
                },
                {
                    "category": "generated",
                    "path": "generated",
                    "rehydrate": "tool build"
                },
                {
                    "category": "package",
                    "path": "release",
                    "retention": "release_asset",
                    "rehydrate": "tool package"
                }
            ]));
            let (_remote, repo) = extension_git_repo();
            write_file(&repo.path().join("cache/untracked.txt"), "user work");
            write_file(&repo.path().join("generated/staged.txt"), "staged work");
            git(repo.path(), &["add", "generated/staged.txt"]);
            write_file(&repo.path().join("release/package.zip"), "release");

            let scan = collect_worktree_candidates(
                &WorktreeInfo {
                    path: repo.path().to_path_buf(),
                },
                &ArtifactCleanupOptions {
                    older_than_days: Some(0),
                    ..Default::default()
                },
                None,
            )
            .expect("safety scan");

            assert!(scan.skipped.iter().any(|row| {
                row.relative_path == "cache" && row.reason.contains("untracked work")
            }));
            assert!(scan.skipped.iter().any(|row| {
                row.relative_path == "generated" && row.reason.contains("tracked or staged")
            }));
            assert!(scan.skipped.iter().any(|row| {
                row.relative_path == "release" && row.reason.contains("release/package")
            }));
        });
    }

    #[test]
    fn extension_apply_is_idempotent_and_reports_allocated_bytes() {
        crate::test_support::with_isolated_home(|_| {
            install_cleanup_extension(serde_json::json!([{
                "category": "dependencies",
                "path": "deps",
                "rehydrate": "tool install"
            }]));
            let (_remote, repo) = extension_git_repo();
            write_file(&repo.path().join(".gitignore"), "deps/\n");
            git(repo.path(), &["add", ".gitignore"]);
            git_commit(repo.path(), "ignore dependencies");
            git(repo.path(), &["push"]);
            write_file(&repo.path().join("deps/package/file"), "artifact bytes");
            let options = ArtifactCleanupOptions {
                path: Some(repo.path().to_path_buf()),
                apply: true,
                older_than_days: Some(0),
                ..Default::default()
            };

            let first = cleanup_artifacts(options.clone()).expect("first apply");
            assert_eq!(first.applied_count, 1);
            assert!(first.estimated_allocated_bytes > 0);
            assert_eq!(
                first.candidates[0].rehydrate.as_deref(),
                Some("tool install")
            );
            assert_eq!(
                first.reclaimed_allocated_bytes,
                first.applied[0].allocated_bytes
            );
            assert!(!repo.path().join("deps").exists());

            let second = cleanup_artifacts(options).expect("second apply");
            assert_eq!(second.applied_count, 0);
            assert_eq!(second.candidate_count, 0);
        });
    }

    #[test]
    fn dry_run_detects_builtin_target_without_homeboy_json() {
        let repo = TempDir::new().expect("repo tempdir");
        git(repo.path(), &["init", "-b", "main"]);
        write_file(
            &repo.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\n",
        );
        write_file(&repo.path().join("src/lib.rs"), "source");
        git(repo.path(), &["add", "Cargo.toml", "src/lib.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        write_file(&repo.path().join("target/debug/app"), "artifact");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: false,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("dry-run cleanup");

        let target = output
            .candidates
            .iter()
            .find(|row| row.relative_path == "target")
            .expect("target candidate");
        assert_eq!(target.kind, "rust_target");
        assert_eq!(target.declared_by, "homeboy:builtin_artifact_paths");
        assert!(repo.path().join("target/debug/app").exists());
    }

    #[test]
    fn worktree_artifact_cleanup_removes_rebuildable_output_and_preserves_source() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "artifact");
        write_file(&repo.path().join("src/lib.rs"), "changed source");

        let output = cleanup_worktree_artifacts(repo.path()).expect("cleanup worktree artifacts");

        assert_eq!(output.worktree_count, 1);
        assert_eq!(output.applied_count, 1);
        assert!(!repo.path().join("target").exists());
        assert_eq!(
            fs::read_to_string(repo.path().join("src/lib.rs")).expect("source remains"),
            "changed source"
        );
    }

    #[test]
    fn clean_contract_dry_run_aggregates_artifacts_and_provider_preview() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "artifact");
        let script = fake_provider_script();

        let output = cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: ResourceCleanupIntent::DryRun,
                artifacts: Some(ArtifactCleanupOptions {
                    path: Some(repo.path().to_path_buf()),
                    apply: true,
                    self_artifacts: false,
                    temp_roots: Vec::new(),
                    sort: ArtifactCleanupSort::Discovery,
                    limit: None,
                    older_than_days: None,
                    merged_only: false,
                }),
                worktree_providers: Some(WorktreeProviderCleanupOptions {
                    provider: vec!["fixture".to_string()],
                    all_providers: false,
                    apply: true,
                }),
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    cleanup_preview: Some(vec![script, "dry_run".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("aggregate dry run cleanup");

        assert_eq!(output.command, "cleanup.resources");
        assert_eq!(output.mode, "dry_run");
        assert_eq!(output.candidate_count, 1);
        assert_eq!(output.applied_count, 0);
        assert_eq!(output.success_count, 1);
        assert_eq!(output.failure_count, 0);
        assert_eq!(output.skipped_count, 0);
        assert_eq!(output.remaining_count, 1);
        assert!(repo.path().join("target/debug/app").exists());
        assert_eq!(
            output
                .worktree_providers
                .as_ref()
                .expect("providers")
                .providers[0]
                .parsed_payload,
            Some(serde_json::json!({ "mode": "dry_run" }))
        );
    }

    #[test]
    fn clean_contract_apply_aggregates_artifact_removal_and_provider_apply() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "artifact");
        let script = fake_provider_script();

        let output = cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: ResourceCleanupIntent::Apply,
                artifacts: Some(ArtifactCleanupOptions {
                    path: Some(repo.path().to_path_buf()),
                    apply: false,
                    self_artifacts: false,
                    temp_roots: Vec::new(),
                    sort: ArtifactCleanupSort::Discovery,
                    limit: None,
                    older_than_days: None,
                    merged_only: false,
                }),
                worktree_providers: Some(WorktreeProviderCleanupOptions {
                    provider: vec!["fixture".to_string()],
                    all_providers: false,
                    apply: false,
                }),
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("aggregate apply cleanup");

        assert_eq!(output.mode, "apply");
        assert_eq!(output.candidate_count, 1);
        assert_eq!(output.applied_count, 1);
        assert_eq!(output.success_count, 1);
        assert_eq!(output.failure_count, 0);
        assert_eq!(output.skipped_count, 0);
        assert_eq!(output.remaining_count, 0);
        assert!(!repo.path().join("target").exists());
        assert_eq!(
            output
                .worktree_providers
                .as_ref()
                .expect("providers")
                .providers[0]
                .parsed_payload,
            Some(serde_json::json!({ "mode": "apply" }))
        );
    }

    #[test]
    fn self_artifact_manifest_must_be_homeboy_crate() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\n",
        )
        .expect("write manifest");

        let err = validate_homeboy_manifest_dir(tmp.path()).expect_err("reject non-homeboy crate");

        assert_eq!(err.code, crate::ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn self_artifact_manifest_rejects_packaged_cargo_registry_source() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write manifest");

        let err = validate_homeboy_manifest_dir(tmp.path()).expect_err("reject packaged source");

        assert_eq!(err.code, crate::ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("is not a Homeboy source git checkout"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("requires a source checkout, not a packaged Cargo registry source")));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("homeboy cleanup artifacts --path <PATH>")));
    }

    #[test]
    fn self_artifact_manifest_resolves_homeboy_git_checkout() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write manifest");
        init_git_repository(tmp.path());

        let root = validate_homeboy_manifest_dir(tmp.path()).expect("homeboy manifest");

        assert_eq!(root, tmp.path());
    }

    #[test]
    fn self_artifact_source_resolves_the_workspace_homeboy_checkout() {
        let root = homeboy_source_checkout().expect("workspace source checkout");

        assert!(root.join(".git").exists());
        assert!(root.join("src/main.rs").is_file());
    }

    #[test]
    fn self_artifact_registry_rejection_suggests_active_checkout_when_discoverable() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write manifest");

        let err = validate_homeboy_manifest_dir(tmp.path()).expect_err("reject packaged source");

        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Active Homeboy checkout appears to be:")));
    }

    #[test]
    fn self_artifacts_cannot_be_combined_with_explicit_path() {
        let tmp = TempDir::new().expect("tempdir");
        let err = resolve_root(&ArtifactCleanupOptions {
            path: Some(tmp.path().to_path_buf()),
            apply: false,
            self_artifacts: true,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect_err("reject ambiguous cleanup root");

        assert_eq!(err.code, crate::ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn cleanup_artifacts_outside_git_checkout_suggests_path_override() {
        let tmp = TempDir::new().expect("tempdir");
        let err = resolve_root(&ArtifactCleanupOptions {
            path: Some(tmp.path().to_path_buf()),
            apply: false,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect_err("reject non-git cleanup root");

        assert_eq!(err.code, crate::ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("not inside a git checkout"));
        assert!(err.message.contains("--path <PATH>"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Run from a git checkout")));
    }

    #[test]
    fn detached_homeboy_temp_artifacts_are_detected_conservatively() {
        let temp_root = TempDir::new().expect("temp root");
        fs::create_dir_all(temp_root.path().join("homeboy-4483-target/debug"))
            .expect("mkdir target artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-target-4318/debug"))
            .expect("mkdir target artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-d6b2bc65-build"))
            .expect("mkdir build artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-runtime-helper-path"))
            .expect("mkdir non-artifact temp");
        fs::create_dir_all(temp_root.path().join("homeboy-main-source-28703209"))
            .expect("mkdir source temp");
        fs::write(
            temp_root
                .path()
                .join("homeboy-main-source-28703209/Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write source manifest");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert_eq!(candidates.len(), 3);
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-4483-target"));
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-target-4318"));
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-d6b2bc65-build"));
        assert!(!candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-runtime-helper-path"));
        assert!(!candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-main-source-28703209"));
    }

    #[test]
    fn apply_removes_detached_temp_artifacts_from_explicit_temp_root() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        write_file(
            &temp_root.path().join("homeboy-4477-target/debug/homeboy"),
            "binary",
        );
        write_file(
            &temp_root
                .path()
                .join("homeboy-main-source-28703209/src/lib.rs"),
            "source",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output
            .candidates
            .iter()
            .any(|row| row.kind == "detached_homeboy_temp_artifact"
                && row.relative_path == "homeboy-4477-target"));
        assert!(!temp_root.path().join("homeboy-4477-target").exists());
        assert!(temp_root
            .path()
            .join("homeboy-main-source-28703209")
            .exists());
    }

    #[test]
    fn temp_homeboy_source_checkout_targets_are_detected_conservatively() {
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-source-28703209");
        write_file(&checkout.join("target/debug/homeboy"), "binary");

        let non_homeboy = temp_root.path().join("homeboy-runtime-helper-path");
        fs::create_dir_all(non_homeboy.join(".git")).expect("mkdir git");
        write_file(
            &non_homeboy.join("Cargo.toml"),
            "[package]\nname = \"other\"\n",
        );
        write_file(&non_homeboy.join("target/debug/other"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("temp artifact candidates");

        let candidate = candidates
            .iter()
            .find(|row| row.kind == "temp_homeboy_checkout_target")
            .expect("homeboy checkout target candidate");
        assert_eq!(candidate.worktree, checkout.to_string_lossy());
        assert_eq!(candidate.path, checkout.join("target").to_string_lossy());
        assert_eq!(candidate.relative_path, "target");
        assert_eq!(candidate.declared_by, "self_temp_root");
        assert!(!candidates
            .iter()
            .any(|row| row.worktree == non_homeboy.to_string_lossy()));
    }

    #[test]
    fn apply_removes_only_target_from_temp_homeboy_source_checkout() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-4447-upgrade-full");
        write_file(&checkout.join("target/debug/homeboy"), "binary");
        write_file(&checkout.join("src/lib.rs"), "changed source");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output.candidates.iter().any(|row| {
            row.kind == "temp_homeboy_checkout_target" && row.worktree == checkout.to_string_lossy()
        }));
        assert!(!checkout.join("target").exists());
        assert!(checkout.join(".git").exists());
        assert_eq!(
            fs::read_to_string(checkout.join("src/lib.rs")).expect("read source"),
            "changed source"
        );
    }

    #[test]
    fn temp_homeboy_source_checkout_target_with_tracked_changes_is_skipped() {
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-4447-upgrade");
        write_file(
            &checkout.join("target/generated.rs"),
            "tracked target source",
        );
        git(&checkout, &["add", "target/generated.rs"]);

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert!(!candidates
            .iter()
            .any(|row| row.kind == "temp_homeboy_checkout_target"));
    }

    #[test]
    fn partial_homeboy_temp_target_is_detected_when_source_skeleton_is_empty() {
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        fs::create_dir_all(partial.join(".github")).expect("mkdir github");
        fs::create_dir_all(partial.join("docs")).expect("mkdir docs");
        fs::create_dir_all(partial.join("src")).expect("mkdir src");
        fs::create_dir_all(partial.join("tests")).expect("mkdir tests");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("temp artifact candidates");

        let candidate = candidates
            .iter()
            .find(|row| row.kind == "partial_homeboy_temp_target")
            .expect("partial temp target candidate");
        assert_eq!(candidate.worktree, partial.to_string_lossy());
        assert_eq!(candidate.path, partial.join("target").to_string_lossy());
        assert_eq!(candidate.relative_path, "target");
    }

    #[test]
    fn partial_homeboy_temp_target_is_skipped_when_source_skeleton_has_content() {
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        write_file(&partial.join("src/lib.rs"), "source");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert!(!candidates
            .iter()
            .any(|row| row.kind == "partial_homeboy_temp_target"));
    }

    #[test]
    fn apply_removes_only_target_from_partial_homeboy_temp() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        fs::create_dir_all(partial.join("src")).expect("mkdir src");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output.candidates.iter().any(|row| {
            row.kind == "partial_homeboy_temp_target" && row.worktree == partial.to_string_lossy()
        }));
        assert!(!partial.join("target").exists());
        assert!(partial.join("src").exists());
    }

    #[test]
    fn dry_run_reports_artifact_candidates_across_worktrees() {
        let repo = git_repo();
        let sibling_parent = TempDir::new().expect("sibling parent");
        let sibling = sibling_parent.path().join("artifact-worktree");
        git(repo.path(), &["worktree", "add", sibling.to_str().unwrap()]);
        write_file(&repo.path().join("target/debug/app"), "primary artifact");
        write_file(
            &sibling.join("node_modules/pkg/index.js"),
            "dependency artifact",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: false,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("dry-run cleanup");

        assert_eq!(output.mode, "dry_run");
        assert_eq!(output.applied_count, 0);
        assert!(output.candidates.iter().any(|row| row
            .worktree
            .ends_with(repo.path().file_name().unwrap().to_str().unwrap())
            && row.relative_path == "target"));
        assert!(output
            .candidates
            .iter()
            .any(|row| row.worktree.ends_with("artifact-worktree")
                && row.relative_path == "node_modules"));
        assert!(repo.path().join("target/debug/app").exists());
        assert!(sibling.join("node_modules/pkg/index.js").exists());
    }

    #[test]
    fn dry_run_can_sort_artifact_candidates_by_size_descending() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "small");
        write_file(&repo.path().join("dist/bundle.js"), &"m".repeat(256));
        write_file(
            &repo.path().join("node_modules/pkg/index.js"),
            &"l".repeat(1024),
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: false,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Size,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("dry-run cleanup");

        let paths: Vec<&str> = output
            .candidates
            .iter()
            .map(|row| row.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["node_modules", "dist", "target"]);
        assert!(output.candidates[0].size_bytes >= output.candidates[1].size_bytes);
        assert!(output.candidates[1].size_bytes >= output.candidates[2].size_bytes);
    }

    #[test]
    fn limit_applies_after_size_sort_and_removes_only_selected_artifacts() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "small");
        write_file(&repo.path().join("dist/bundle.js"), &"m".repeat(256));
        write_file(
            &repo.path().join("node_modules/pkg/index.js"),
            &"l".repeat(1024),
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Size,
            limit: Some(2),
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        let paths: Vec<&str> = output
            .candidates
            .iter()
            .map(|row| row.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["node_modules", "dist"]);
        assert_eq!(output.candidate_count, 2);
        assert_eq!(output.applied_count, 2);
        assert!(!repo.path().join("node_modules").exists());
        assert!(!repo.path().join("dist").exists());
        assert!(repo.path().join("target/debug/app").exists());
    }

    #[test]
    fn apply_removes_declared_artifacts_only_and_preserves_dirty_source() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "artifact");
        write_file(&repo.path().join("src/lib.rs"), "changed source");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        assert_eq!(output.mode, "apply");
        assert_eq!(output.applied_count, 1);
        assert!(!repo.path().join("target").exists());
        assert_eq!(
            fs::read_to_string(repo.path().join("src/lib.rs")).expect("read source"),
            "changed source"
        );
        assert!(output.candidates.iter().any(|row| row.source_dirty));
    }

    #[test]
    fn apply_reports_remaining_and_cumulative_session_totals_across_retries() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "first");

        let first = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("first apply cleanup");

        assert_eq!(
            first.summary.invocation_reclaimed_bytes,
            first.reclaimed_bytes
        );
        assert_eq!(first.summary.previous_session_reclaimed_bytes, 0);
        assert_eq!(first.summary.remaining_candidate_count, 0);
        assert_eq!(first.summary.remaining_candidate_bytes, 0);
        assert_eq!(
            first.summary.cumulative_session_reclaimed_bytes,
            first.reclaimed_bytes
        );

        write_file(&repo.path().join("node_modules/pkg/index.js"), "second");

        let second = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("second apply cleanup");

        assert_eq!(
            second.summary.invocation_reclaimed_bytes,
            second.reclaimed_bytes
        );
        assert_eq!(
            second.summary.previous_session_reclaimed_bytes,
            first.summary.cumulative_session_reclaimed_bytes
        );
        assert_eq!(
            second.summary.cumulative_session_reclaimed_bytes,
            first.reclaimed_bytes + second.reclaimed_bytes
        );
        assert_eq!(second.summary.remaining_candidate_count, 0);
        assert_eq!(second.summary.remaining_candidate_bytes, 0);
        assert!(second.summary.session_state_path.is_some());
        assert_eq!(second.summary.session_state_error, None);
    }

    #[test]
    fn artifact_cleanup_result_counts_satisfy_outcome_invariants() {
        let cases = [
            // all-success
            (3, 3, 0, 3, 0),
            // partial failure: failures remain and are not reported as successes
            (3, 1, 2, 1, 2),
            // dry-run: candidates remain untouched
            (3, 0, 0, 0, 3),
            // no candidates
            (0, 0, 0, 0, 0),
        ];

        for (candidates, applied, failures, expected_successes, expected_remaining) in cases {
            let (successes, remaining) =
                artifact_cleanup_result_counts(candidates, applied, failures);

            assert_eq!(successes, expected_successes);
            assert_eq!(remaining, expected_remaining);
            assert_eq!(applied, successes);
            assert_eq!(candidates, successes + remaining);
            assert!(failures <= remaining);
        }
    }

    #[test]
    fn artifact_cleanup_reports_partial_removal_failures_without_aborting() {
        let candidates = vec![
            artifact_candidate("target"),
            artifact_candidate("dist"),
            artifact_candidate("node_modules"),
        ];
        let (applied, failed) = apply_artifact_candidates(&candidates, |candidate| match candidate
            .relative_path
            .as_str()
        {
            "target" => Some(Ok(())),
            "dist" => Some(Err(Error::internal_unexpected("remove failed"))),
            _ => None,
        });
        let (success_count, remaining_count) =
            artifact_cleanup_result_counts(candidates.len(), applied.len(), failed.len());

        assert_eq!(applied.len(), 1);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].relative_path, "dist");
        assert_eq!(success_count, 1);
        assert_eq!(remaining_count, 2);
    }

    #[test]
    fn apply_skips_artifact_path_with_tracked_source_changes() {
        let repo = git_repo();
        write_file(
            &repo.path().join("target/generated.rs"),
            "tracked artifact source",
        );
        git(repo.path(), &["add", "target/generated.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "track generated source",
            ],
        );
        write_file(
            &repo.path().join("target/generated.rs"),
            "modified tracked source",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: false,
        })
        .expect("apply cleanup");

        assert_eq!(output.applied_count, 0);
        assert!(repo.path().join("target/generated.rs").exists());
        assert!(output.skipped.iter().any(|row| {
            row.relative_path == "target" && row.reason.contains("tracked or staged source changes")
        }));
    }

    #[test]
    fn artifact_directory_removal_retries_transient_non_empty_errors() {
        let artifact = PathBuf::from("target");
        let mut attempts = 0;
        let mut sleeps = Vec::new();

        remove_artifact_directory_with(
            &artifact,
            |_| {
                attempts += 1;
                if attempts == 1 {
                    Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty))
                } else {
                    Ok(())
                }
            },
            |duration| sleeps.push(duration),
        )
        .expect("transient non-empty directory removal should retry");

        assert_eq!(attempts, 2);
        assert_eq!(sleeps, vec![ARTIFACT_DIR_REMOVE_RETRY_DELAY]);
    }

    #[test]
    fn artifact_directory_removal_reports_persistent_non_empty_errors() {
        let artifact = PathBuf::from("target");
        let mut attempts = 0;

        let err = remove_artifact_directory_with(
            &artifact,
            |_| {
                attempts += 1;
                Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty))
            },
            |_| {},
        )
        .expect_err("persistent non-empty directory removal should fail");

        assert_eq!(attempts, ARTIFACT_DIR_REMOVE_ATTEMPTS);
        assert_eq!(err.code, crate::ErrorCode::InternalIoError);
    }

    #[test]
    fn artifact_directory_removal_tolerates_already_removed_artifact() {
        let artifact = PathBuf::from("target");

        remove_artifact_directory_with(
            &artifact,
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
            |_| {},
        )
        .expect("already removed artifact should be treated as removed");
    }

    #[test]
    fn branch_is_merged_detects_ancestor_and_unmerged_worktrees() {
        // upstream "remote" repo
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let merged = git_repo();
        git(merged.path(), &["remote", "add", "origin", &remote_url]);
        git(merged.path(), &["push", "-u", "origin", "main"]);
        // No commits ahead of upstream → merged (ancestor case).
        assert!(branch_is_merged(merged.path()));

        // Add a local commit that has not been pushed → not merged.
        write_file(&merged.path().join("src/feature.rs"), "feature");
        git(merged.path(), &["add", "src/feature.rs"]);
        git(
            merged.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "unmerged feature",
            ],
        );
        assert!(!branch_is_merged(merged.path()));
    }

    #[test]
    fn branch_is_merged_false_without_upstream() {
        let repo = git_repo();
        // No tracking branch configured at all.
        assert!(!branch_is_merged(repo.path()));
    }

    #[test]
    fn merged_only_preserves_unmerged_worktree_target() {
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let repo = git_repo();
        git(repo.path(), &["remote", "add", "origin", &remote_url]);
        git(repo.path(), &["push", "-u", "origin", "main"]);

        // Local unmerged commit → branch is ahead of upstream.
        write_file(&repo.path().join("src/feature.rs"), "feature");
        git(repo.path(), &["add", "src/feature.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "unmerged feature",
            ],
        );
        write_file(&repo.path().join("target/debug/app"), "artifact");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: true,
        })
        .expect("merged-only cleanup");

        assert_eq!(output.applied_count, 0, "unmerged target must be preserved");
        assert!(repo.path().join("target/debug/app").exists());
        assert!(output.skipped.iter().any(|row| {
            row.relative_path == "target" && row.reason.contains("not merged into its upstream")
        }));
    }

    #[test]
    fn merged_only_reclaims_merged_worktree_target() {
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let repo = git_repo();
        git(repo.path(), &["remote", "add", "origin", &remote_url]);
        git(repo.path(), &["push", "-u", "origin", "main"]);

        // Branch tip equals upstream → merged. Leftover target/ should be reclaimed.
        write_file(&repo.path().join("target/debug/app"), "artifact");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            older_than_days: None,
            merged_only: true,
        })
        .expect("merged-only cleanup");

        assert!(output.applied_count >= 1, "merged target must be reclaimed");
        assert!(!repo.path().join("target").exists());
    }

    #[test]
    fn artifact_cleanup_preview_apply_command_preserves_reviewed_scope() {
        let options = ArtifactCleanupOptions {
            path: Some(PathBuf::from("/tmp/review scope")),
            apply: false,
            self_artifacts: false,
            temp_roots: vec![
                PathBuf::from("/tmp/first root"),
                PathBuf::from("/tmp/second"),
            ],
            sort: ArtifactCleanupSort::Size,
            limit: Some(7),
            older_than_days: Some(14),
            merged_only: true,
        };

        assert_eq!(
            artifact_cleanup_apply_command(&options),
            "homeboy cleanup artifacts --path '/tmp/review scope' --temp-root '/tmp/first root' --temp-root /tmp/second --sort size --limit 7 --older-than-days 14 --merged-only --apply"
        );

        assert_eq!(
            artifact_cleanup_apply_command(&ArtifactCleanupOptions {
                path: None,
                self_artifacts: true,
                ..options
            }),
            "homeboy cleanup artifacts --self --temp-root '/tmp/first root' --temp-root /tmp/second --sort size --limit 7 --older-than-days 14 --merged-only --apply"
        );
    }

    fn git_repo() -> TempDir {
        let repo = TempDir::new().expect("repo tempdir");
        git(repo.path(), &["init", "-b", "main"]);
        write_file(&repo.path().join("src/lib.rs"), "source");
        write_file(
            &repo.path().join("homeboy.json"),
            r#"{"artifact_cleanup_paths":["target","node_modules","dist"]}"#,
        );
        git(repo.path(), &["add", "src/lib.rs", "homeboy.json"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        repo
    }

    fn install_cleanup_extension(rules: serde_json::Value) {
        let extension_dir = crate::paths::extensions()
            .expect("extensions path")
            .join("fixture");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("fixture.json"),
            serde_json::json!({
                "name": "Fixture",
                "version": "1.0.0",
                "build": { "artifact_cleanup": rules }
            })
            .to_string(),
        )
        .expect("extension manifest");
    }

    fn extension_git_repo() -> (TempDir, TempDir) {
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let repo = TempDir::new().expect("repo");
        git(repo.path(), &["init", "-b", "main"]);
        write_file(&repo.path().join("src/lib.rs"), "source");
        write_file(
            &repo.path().join("homeboy.json"),
            r#"{"id":"fixture","extensions":{"fixture":{}}}"#,
        );
        git(repo.path(), &["add", "src/lib.rs", "homeboy.json"]);
        git_commit(repo.path(), "initial");
        let remote_url = remote.path().to_string_lossy().to_string();
        git(repo.path(), &["remote", "add", "origin", &remote_url]);
        git(repo.path(), &["push", "-u", "origin", "main"]);
        (remote, repo)
    }

    fn git_commit(path: &Path, message: &str) {
        git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn init_git_repository(path: &Path) {
        git(path, &["init", "-b", "main"]);
    }

    fn config_with_provider(provider: WorktreeProviderConfig) -> HomeboyConfig {
        let mut providers = HashMap::new();
        providers.insert("fixture".to_string(), provider);
        HomeboyConfig {
            worktree_providers: providers,
            ..HomeboyConfig::default()
        }
    }

    /// Shared, process-wide root for fixture provider scripts.
    ///
    /// A fixture script must outlive the helper that writes it (the test runs it
    /// later), but previously each call `.keep()`-ed its own `tempfile::tempdir()`,
    /// permanently disabling `TempDir` cleanup and leaking a directory per run
    /// (see #9173 follow-up). Anchor all fixture scripts under a single `TempDir`
    /// owned by this `OnceLock`: created once, cleaned up on normal process exit,
    /// and `hb-test-` prefixed so the startup sweep (#9177) reclaims it even if
    /// the process is killed.
    fn fixture_script_root() -> &'static Path {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            tempfile::Builder::new()
                .prefix("hb-test-cleanup-fixtures-")
                .tempdir()
                .expect("fixture script root tempdir")
        })
        .path()
    }

    fn unique_fixture_script_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = fixture_script_root().join(format!("fixture-{id}"));
        fs::create_dir_all(&dir).expect("create fixture script dir");
        dir
    }

    fn fake_provider_script() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nprintf '{\"mode\":\"%s\"}\n' \"$1\"\n")
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}

    fn temp_homeboy_checkout(temp_root: &Path, name: &str) -> PathBuf {
        let checkout = temp_root.join(name);
        fs::create_dir_all(&checkout).expect("mkdir checkout");
        git(&checkout, &["init", "-b", "main"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ],
        );
        write_file(
            &checkout.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        );
        write_file(&checkout.join("src/lib.rs"), "source");
        git(&checkout, &["add", "Cargo.toml", "src/lib.rs"]);
        git(
            &checkout,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        checkout
    }

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir parent");
        fs::write(path, content).expect("write file");
    }

    fn artifact_candidate(relative_path: &str) -> ArtifactCleanupCandidate {
        ArtifactCleanupCandidate {
            worktree: "/repo".to_string(),
            path: format!("/repo/{relative_path}"),
            relative_path: relative_path.to_string(),
            kind: "artifact".to_string(),
            declared_by: "test".to_string(),
            category: None,
            rehydrate: None,
            size_bytes: 1,
            allocated_bytes: 1,
            age_seconds: None,
            liveness: "not_required".to_string(),
            source_dirty: false,
            unpushed_commits: false,
            extension_owned: false,
            minimum_age_days: 0,
        }
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

    #[test]
    fn non_git_worktree_scan_errors_without_aborting_batch() {
        // A stale/non-Git worktree path makes the per-worktree scan fail, but
        // the batch loop turns that into a skip rather than aborting (#9925).
        let not_a_repo = TempDir::new().expect("non-git dir");
        let scan = collect_worktree_candidates(
            &WorktreeInfo {
                path: not_a_repo.path().to_path_buf(),
            },
            &ArtifactCleanupOptions::default(),
            None,
        );
        assert!(
            scan.is_err(),
            "a non-Git worktree scan should fail so the caller can skip it"
        );
    }

    #[test]
    fn one_invalid_worktree_does_not_block_cleanup_of_valid_worktrees() {
        // Batch with a valid git worktree (declaring a target/ artifact) plus a
        // stale non-Git worktree. The valid worktree must still yield a
        // candidate, and the invalid one must be reported as skipped -- not
        // abort the whole batch.
        let valid = git_repo();
        write_file(&valid.path().join("target/debug/build.o"), "artifact bytes");
        let invalid = TempDir::new().expect("non-git dir");

        let output = cleanup_artifacts_in_worktrees(
            valid.path().to_path_buf(),
            vec![
                WorktreeInfo {
                    path: valid.path().to_path_buf(),
                },
                WorktreeInfo {
                    path: invalid.path().to_path_buf(),
                },
            ],
            &ArtifactCleanupOptions::default(),
            false,
        )
        .expect("batch must not abort on one bad worktree");

        assert!(
            output
                .candidates
                .iter()
                .any(|candidate| candidate.relative_path == "target"),
            "valid worktree's target/ artifact should be a candidate: {:?}",
            output.candidates
        );
        assert!(
            output
                .skipped
                .iter()
                .any(|skip| skip.worktree == invalid.path().to_string_lossy()
                    && skip.reason.contains("could not be inspected")),
            "invalid worktree should be reported as skipped: {:?}",
            output.skipped
        );
    }
}
