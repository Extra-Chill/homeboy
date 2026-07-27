use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
mod extension_declarations;
mod self_artifacts;

use extension_declarations::extension_artifact_declarations;
#[cfg(test)]
use self_artifacts::validate_homeboy_manifest_dir;
use self_artifacts::{homeboy_source_checkout, self_temp_artifact_candidates};

const ARTIFACT_DIR_REMOVE_ATTEMPTS: usize = 3;
const ARTIFACT_DIR_REMOVE_RETRY_DELAY: Duration = Duration::from_millis(50);
const BUILTIN_ARTIFACT_PATHS: &[(&str, &str)] = &[("target", "rust_target")];
const SECONDS_PER_DAY: u64 = 86_400;

/// Default artifact class for declarations that predate categorized cleanup.
/// Both the built-in path and repository-declared paths describe output that a
/// build regenerates, so they report as build output.
const LEGACY_ARTIFACT_CATEGORY: &str = "build_output";

/// Liveness of the checkout an artifact belongs to.
const LIVENESS_ACTIVE: &str = "active_task_worktree";
const LIVENESS_IDLE: &str = "idle";

/// What the checkout needs before it is usable again once the artifact is gone.
const READINESS_REHYDRATION_REQUIRED: &str = "rehydration_required";
const READINESS_REBUILD_ON_DEMAND: &str = "rebuild_on_demand";

/// Homeboy's own scratch build output: never a registered task worktree, and
/// always rebuilt by the next build rather than reinstalled.
pub(crate) const SELF_TEMP_ARTIFACT_CATEGORY: &str = LEGACY_ARTIFACT_CATEGORY;
pub(crate) const SELF_TEMP_ARTIFACT_LIVENESS: &str = LIVENESS_IDLE;
pub(crate) const SELF_TEMP_ARTIFACT_READINESS: &str = READINESS_REBUILD_ON_DEMAND;

#[derive(Debug, Clone, Default)]
pub struct ArtifactCleanupOptions {
    pub path: Option<PathBuf>,
    pub apply: bool,
    pub self_artifacts: bool,
    pub temp_roots: Vec<PathBuf>,
    pub sort: ArtifactCleanupSort,
    pub limit: Option<usize>,
    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream (ancestor or patch-equivalent / squash-merged). This
    /// keeps in-progress cooks' build dirs intact while reclaiming the large
    /// `target/` dirs left behind by merged worktrees.
    pub merged_only: bool,
    /// Require artifacts to be untouched for at least this many days. Composes
    /// with any age floor an extension declaration sets; the stricter wins.
    pub min_age_days: Option<u64>,
    /// Reclaim extension-declared artifacts even from checkouts registered as
    /// active task worktrees. Those declarations are protected by default
    /// because removing an install tree makes a live checkout unusable until it
    /// is rehydrated.
    pub include_active_worktrees: bool,
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
    pub reclaimed_allocated_bytes: u64,
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
    pub reclaimed_bytes: u64,
    /// Disk actually charged to the artifacts, not the sum of apparent file
    /// sizes. Dependency trees are dominated by small files, so allocated bytes
    /// are what an operator watching free space actually gets back.
    pub estimated_allocated_bytes: u64,
    pub reclaimed_allocated_bytes: u64,
    /// Replays the reviewed cleanup scope with mutation explicitly enabled.
    pub next_command: String,
    pub summary: ArtifactCleanupSummary,
    pub worktrees: Vec<ArtifactCleanupWorktreeSummary>,
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

/// Per-checkout roll-up. Aggregate cleanup spans many worktrees, and the unit an
/// operator acts on is the checkout: how much it is holding, and the command
/// that puts it back to work.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupWorktreeSummary {
    pub worktree: String,
    pub liveness: String,
    pub candidate_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub estimated_allocated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub reclaimed_allocated_bytes: u64,
    /// Commands that rehydrate what this invocation would remove or removed.
    pub rehydrate_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupCandidate {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub declared_by: String,
    /// Artifact class reported by the declaration owner.
    pub category: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    /// Seconds since the newest write anywhere inside the artifact.
    pub age_seconds: Option<u64>,
    pub liveness: String,
    pub readiness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rehydrate_command: Option<String>,
    pub source_dirty: bool,
    pub unpushed_commits: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupSkipped {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub declared_by: String,
    pub category: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupApplied {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub category: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rehydrate_command: Option<String>,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupFailed {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub category: String,
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
    /// Artifact class. Only reconstructable classes are removal candidates.
    pub category: String,
    pub reconstructable: bool,
    pub rehydrate_command: Option<String>,
    /// Age floor the declaration owner requires before removal.
    pub min_age_days: Option<u64>,
    /// Whether an active task worktree protects this declaration by default.
    pub liveness_protected: bool,
}

impl ArtifactDeclaration {
    fn readiness(&self) -> &'static str {
        if self.rehydrate_command.is_some() {
            READINESS_REHYDRATION_REQUIRED
        } else {
            READINESS_REBUILD_ON_DEMAND
        }
    }
}

#[derive(Debug, Default)]
struct GitSafety {
    source_dirty: bool,
    unpushed_commits: bool,
    dirty_paths: Vec<String>,
    /// Untracked, non-ignored entries. An artifact path holding these is not
    /// purely reconstructable output, so it is never removed.
    untracked_paths: Vec<String>,
}

/// Paths of checkouts Homeboy currently records as active task worktrees.
/// Resolved once per invocation so the registry is read a single time.
#[derive(Debug, Default, Clone)]
struct ActiveWorktrees {
    paths: HashSet<PathBuf>,
}

impl ActiveWorktrees {
    fn resolve() -> Self {
        let Ok(listing) = crate::worktree::list() else {
            return Self::default();
        };
        let paths = listing
            .worktrees
            .into_iter()
            .filter(|record| record.state == crate::worktree::TaskWorktreeState::Active)
            .map(|record| canonical_or_owned(Path::new(&record.worktree_path)))
            .collect();
        Self { paths }
    }

    fn liveness(&self, worktree: &Path) -> &'static str {
        if self.paths.contains(&canonical_or_owned(worktree)) {
            LIVENESS_ACTIVE
        } else {
            LIVENESS_IDLE
        }
    }
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
    active: &ActiveWorktrees,
) -> Result<WorktreeCandidateScan> {
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    let safety = git_safety(&worktree.path)?;
    let liveness = active.liveness(&worktree.path);
    if options.merged_only && !branch_is_merged(&worktree.path) {
        for declaration in artifact_declarations(&worktree.path)? {
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
    for declaration in artifact_declarations(&worktree.path)? {
        let artifact_path = worktree.path.join(&declaration.relative_path);
        let display_path = artifact_path.to_string_lossy().to_string();
        if !artifact_path.exists() {
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
        if !declaration.reconstructable {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "declared artifact is a release asset retained for deployment",
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
        if has_untracked_work_at(&safety.untracked_paths, &declaration.relative_path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "artifact path holds untracked work that Git does not ignore",
            ));
            continue;
        }
        if tracks_files_under(&worktree.path, &declaration.relative_path) {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "artifact path contains files tracked by Git",
            ));
            continue;
        }
        if declaration.liveness_protected
            && !options.include_active_worktrees
            && liveness == LIVENESS_ACTIVE
        {
            skipped.push(skip_row(
                worktree,
                &declaration,
                display_path,
                "checkout is a registered active task worktree",
            ));
            continue;
        }

        let usage = path_usage(&artifact_path)?;
        let age_seconds = usage.age_seconds();
        if let Some(min_age_days) = effective_min_age_days(options, &declaration) {
            if !meets_age_gate(age_seconds, min_age_days) {
                skipped.push(skip_row(
                    worktree,
                    &declaration,
                    display_path,
                    &format!("artifact was modified within the {min_age_days}-day age gate"),
                ));
                continue;
            }
        }

        candidates.push(ArtifactCleanupCandidate {
            worktree: worktree.path.to_string_lossy().to_string(),
            path: display_path.clone(),
            relative_path: declaration.relative_path.clone(),
            kind: declaration.kind.clone(),
            declared_by: declaration.declared_by.clone(),
            category: declaration.category.clone(),
            size_bytes: usage.logical_bytes,
            allocated_bytes: usage.allocated_bytes,
            age_seconds,
            liveness: liveness.to_string(),
            readiness: declaration.readiness().to_string(),
            rehydrate_command: declaration.rehydrate_command.clone(),
            source_dirty: safety.source_dirty,
            unpushed_commits: safety.unpushed_commits,
        });
    }

    Ok(WorktreeCandidateScan {
        candidates,
        skipped,
    })
}

/// The strictest age floor in play: the caller's gate and the declaration
/// owner's gate both have to pass.
fn effective_min_age_days(
    options: &ArtifactCleanupOptions,
    declaration: &ArtifactDeclaration,
) -> Option<u64> {
    match (options.min_age_days, declaration.min_age_days) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// An artifact whose age cannot be read fails the gate. An unreadable timestamp
/// is not evidence of staleness, and the gate exists to protect recent work.
fn meets_age_gate(age_seconds: Option<u64>, min_age_days: u64) -> bool {
    age_seconds.is_some_and(|age| age >= min_age_days.saturating_mul(SECONDS_PER_DAY))
}

/// A worktree-level skip row (no specific artifact declaration), used when an
/// entire worktree cannot be inspected.
fn worktree_skip_row(worktree: &WorktreeInfo, reason: String) -> ArtifactCleanupSkipped {
    ArtifactCleanupSkipped {
        worktree: worktree.path.to_string_lossy().to_string(),
        path: worktree.path.to_string_lossy().to_string(),
        relative_path: String::new(),
        kind: String::new(),
        declared_by: String::new(),
        category: String::new(),
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
    let active = ActiveWorktrees::resolve();

    for worktree in &worktrees {
        // A single stale/non-Git/vanished worktree candidate must not abort the
        // whole batch: classify it, record a bounded diagnostic, and continue so
        // independent valid worktrees are still cleaned (#9925).
        match collect_worktree_candidates(worktree, options, &active) {
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
            path.exists().then(|| remove_artifact_path(path))
        })
    } else {
        (Vec::new(), Vec::new())
    };

    let estimated_bytes = candidates.iter().map(|row| row.size_bytes).sum();
    let reclaimed_bytes = applied.iter().map(|row| row.size_bytes).sum();
    let estimated_allocated_bytes = candidates.iter().map(|row| row.allocated_bytes).sum();
    let reclaimed_allocated_bytes = applied.iter().map(|row| row.allocated_bytes).sum();
    let worktree_rows = worktree_summaries(&candidates, &skipped, &applied, &active);
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
        reclaimed_bytes,
        estimated_allocated_bytes,
        reclaimed_allocated_bytes,
        next_command: artifact_cleanup_apply_command(options),
        summary,
        worktrees: worktree_rows,
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
    if options.merged_only {
        command.push_str(" --merged-only");
    }
    if let Some(min_age_days) = options.min_age_days {
        command.push_str(&format!(" --min-age-days {min_age_days}"));
    }
    if options.include_active_worktrees {
        command.push_str(" --include-active-worktrees");
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
    let reclaimed_allocated_bytes = artifacts
        .as_ref()
        .map(|output| output.reclaimed_allocated_bytes)
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
        reclaimed_allocated_bytes,
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
            category: LEGACY_ARTIFACT_CATEGORY.to_string(),
            reconstructable: true,
            rehydrate_command: None,
            min_age_days: None,
            liveness_protected: false,
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
                category: LEGACY_ARTIFACT_CATEGORY.to_string(),
                reconstructable: true,
                rehydrate_command: None,
                min_age_days: None,
                liveness_protected: false,
            });
        }
    }

    // Extension-owned declarations resolve last: a repository that names a path
    // explicitly has already decided how that path is treated, and the
    // ecosystem-wide rule must not override the repository-local one.
    declarations.extend(extension_artifact_declarations(worktree));

    let mut seen = HashSet::new();
    declarations.retain(|row| seen.insert(row.relative_path.clone()));
    Ok(declarations)
}

fn git_safety(worktree: &Path) -> Result<GitSafety> {
    let status = git::run_git(worktree, &["status", "--porcelain=v1"], "git status")?;
    let mut dirty_paths = Vec::new();
    let mut source_dirty = false;
    for line in status.lines() {
        if line.len() < 4 || line.starts_with("?? ") || line.starts_with("!! ") {
            continue;
        }
        let path = status_path(line);
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
        untracked_paths: untracked_paths(worktree),
    })
}

/// Untracked entries Git does not ignore, collapsed to whole directories so a
/// large untracked tree stays one row instead of a per-file listing.
///
/// Declared artifacts are normally ignored, so this list is empty in the common
/// case. When it is not, the path holds files nothing else is tracking — that is
/// work, not reconstructable output, and cleanup leaves it alone.
fn untracked_paths(worktree: &Path) -> Vec<String> {
    let output = git::run_git(
        worktree,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
        ],
        "git ls-files untracked",
    );
    match output {
        Ok(listing) => listing
            .lines()
            .map(|line| {
                line.trim()
                    .trim_matches('"')
                    .trim_end_matches('/')
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
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

/// Whether Git tracks anything inside the artifact path.
///
/// A committed artifact tree is content of record, not reconstructable output,
/// even when it is clean — a clean tree produces no status entry, so the
/// dirty-path guard alone would happily delete it. Declarations that match a
/// path some repository commits (generated output checked in on purpose) are
/// left alone instead of quietly rewriting that repository's working tree.
///
/// Scoped to one pathspec and evaluated only for paths that already passed the
/// cheaper gates, so this stays a small number of calls per checkout.
fn tracks_files_under(worktree: &Path, relative_path: &str) -> bool {
    match git::run_git(
        worktree,
        &["ls-files", "--", relative_path],
        "git ls-files tracked",
    ) {
        Ok(listing) => !listing.trim().is_empty(),
        Err(_) => false,
    }
}

/// Whether untracked, non-ignored work sits at, inside, or above the artifact
/// path.
///
/// The ancestor case matters as much as the descendant case: an artifact nested
/// inside an untracked directory is part of a tree Git is not accounting for,
/// and removing it would delete work nothing else records.
fn has_untracked_work_at(untracked_paths: &[String], relative_path: &str) -> bool {
    let artifact = relative_path.trim_end_matches('/');
    untracked_paths.iter().any(|path| {
        let path = path.trim_end_matches('/');
        path == artifact
            || path.starts_with(&format!("{artifact}/"))
            || artifact.starts_with(&format!("{path}/"))
    })
}

fn applied_row(candidate: &ArtifactCleanupCandidate) -> ArtifactCleanupApplied {
    ArtifactCleanupApplied {
        worktree: candidate.worktree.clone(),
        path: candidate.path.clone(),
        relative_path: candidate.relative_path.clone(),
        kind: candidate.kind.clone(),
        category: candidate.category.clone(),
        size_bytes: candidate.size_bytes,
        allocated_bytes: candidate.allocated_bytes,
        rehydrate_command: candidate.rehydrate_command.clone(),
        removed: true,
    }
}

fn failed_row(candidate: &ArtifactCleanupCandidate, error: String) -> ArtifactCleanupFailed {
    ArtifactCleanupFailed {
        worktree: candidate.worktree.clone(),
        path: candidate.path.clone(),
        relative_path: candidate.relative_path.clone(),
        kind: candidate.kind.clone(),
        category: candidate.category.clone(),
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
        declared_by: declaration.declared_by.clone(),
        category: declaration.category.clone(),
        reason: reason.to_string(),
    }
}

/// Roll candidates, skips, and applied rows up per checkout.
///
/// Rehydration guidance is deduplicated per checkout: an operator needs the set
/// of commands that restores the checkout, not one line per removed path.
fn worktree_summaries(
    candidates: &[ArtifactCleanupCandidate],
    skipped: &[ArtifactCleanupSkipped],
    applied: &[ArtifactCleanupApplied],
    active: &ActiveWorktrees,
) -> Vec<ArtifactCleanupWorktreeSummary> {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for worktree in candidates
        .iter()
        .map(|row| &row.worktree)
        .chain(skipped.iter().map(|row| &row.worktree))
    {
        if seen.insert(worktree.clone()) {
            order.push(worktree.clone());
        }
    }

    order
        .into_iter()
        .map(|worktree| {
            let owned: Vec<_> = candidates
                .iter()
                .filter(|row| row.worktree == worktree)
                .collect();
            let removed: Vec<_> = applied
                .iter()
                .filter(|row| row.worktree == worktree)
                .collect();
            let mut rehydrate_commands = Vec::new();
            for command in owned.iter().filter_map(|row| row.rehydrate_command.clone()) {
                if !rehydrate_commands.contains(&command) {
                    rehydrate_commands.push(command);
                }
            }

            ArtifactCleanupWorktreeSummary {
                liveness: active.liveness(Path::new(&worktree)).to_string(),
                candidate_count: owned.len(),
                skipped_count: skipped
                    .iter()
                    .filter(|row| row.worktree == worktree)
                    .count(),
                estimated_bytes: owned.iter().map(|row| row.size_bytes).sum(),
                estimated_allocated_bytes: owned.iter().map(|row| row.allocated_bytes).sum(),
                reclaimed_bytes: removed.iter().map(|row| row.size_bytes).sum(),
                reclaimed_allocated_bytes: removed.iter().map(|row| row.allocated_bytes).sum(),
                rehydrate_commands,
                worktree,
            }
        })
        .collect()
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

/// What one artifact path costs and how recently it was touched.
///
/// Apparent size and allocated size diverge sharply for large trees of small
/// files: apparent size answers "how much content", allocated size answers "how
/// much disk comes back". Both are reported so an operator can reconcile a
/// cleanup report against free space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PathUsage {
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) newest_modified: Option<SystemTime>,
}

impl PathUsage {
    fn merge(&mut self, other: PathUsage) {
        self.logical_bytes = self.logical_bytes.saturating_add(other.logical_bytes);
        self.allocated_bytes = self.allocated_bytes.saturating_add(other.allocated_bytes);
        self.newest_modified = match (self.newest_modified, other.newest_modified) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
    }

    /// Seconds since the newest write anywhere in the tree. `None` when no
    /// timestamp could be read, or when the newest write is in the future.
    pub(crate) fn age_seconds(&self) -> Option<u64> {
        let newest = self.newest_modified?;
        SystemTime::now()
            .duration_since(newest)
            .ok()
            .map(|elapsed| elapsed.as_secs())
    }
}

fn path_size(path: &Path) -> Result<u64> {
    Ok(path_usage(path)?.logical_bytes)
}

pub(crate) fn path_usage(path: &Path) -> Result<PathUsage> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    let modified = metadata.modified().ok();
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(PathUsage {
            logical_bytes: metadata.len(),
            allocated_bytes: allocated_bytes(&metadata, metadata.len()),
            newest_modified: modified,
        });
    }

    // Sum only the reclaimable file/symlink content. A directory's own
    // `metadata.len()` is the inode/directory-entry size (typically a 4 KiB
    // block on ext4/tmpfs), not reclaimable payload — counting it made size
    // sorting reflect directory nesting depth rather than actual artifact
    // weight (e.g. a 5-byte file under two nested dirs outranking a 256-byte
    // file under one). Recurse over children and count their bytes only.
    //
    // Allocated bytes do include directory blocks: removing the tree frees the
    // directory inodes too, and a deep tree of small files can hold a
    // meaningful share of its footprint there.
    let mut usage = PathUsage {
        logical_bytes: 0,
        allocated_bytes: allocated_bytes(&metadata, 0),
        newest_modified: modified,
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
        usage.merge(path_usage(&entry.path())?);
    }
    Ok(usage)
}

/// Disk actually charged to an entry. Falls back to apparent size on platforms
/// that do not report block allocation.
#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata, _fallback: u64) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_metadata: &fs::Metadata, fallback: u64) -> u64 {
    fallback
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
    fn untracked_work_is_detected_at_inside_and_above_the_artifact_path() {
        let untracked = vec!["modules".to_string(), "build/notes.txt".to_string()];

        assert!(
            has_untracked_work_at(&untracked, "modules/alpha/deps"),
            "an artifact nested inside an untracked tree is protected"
        );
        assert!(
            has_untracked_work_at(&untracked, "modules"),
            "an artifact path that is itself untracked is protected"
        );
        assert!(
            has_untracked_work_at(&untracked, "build"),
            "untracked work inside the artifact path is protected"
        );
        assert!(!has_untracked_work_at(&untracked, "target"));
        assert!(
            !has_untracked_work_at(&untracked, "modules-generated"),
            "a shared name prefix is not a path prefix"
        );
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
        crate::test_support::with_isolated_home(|_| {
            let tmp = TempDir::new().expect("tempdir");

            let declarations = artifact_declarations(tmp.path()).expect("declarations");

            assert_eq!(declarations.len(), 1);
            assert_eq!(declarations[0].relative_path, "target");
            assert_eq!(declarations[0].kind, "rust_target");
            assert_eq!(
                declarations[0].declared_by,
                "homeboy:builtin_artifact_paths"
            );
            assert_eq!(declarations[0].category, LEGACY_ARTIFACT_CATEGORY);
            assert!(declarations[0].reconstructable);
            assert!(!declarations[0].liveness_protected);
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
        write_file(&repo.path().join(".gitignore"), "target/\n");
        git(
            repo.path(),
            &["add", "Cargo.toml", "src/lib.rs", ".gitignore"],
        );
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
                    merged_only: false,
                    min_age_days: None,
                    include_active_worktrees: false,
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
                    merged_only: false,
                    min_age_days: None,
                    include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
        git(repo.path(), &["add", "--force", "target/generated.rs"]);
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
            merged_only: false,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: true,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: true,
            min_age_days: None,
            include_active_worktrees: false,
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
            merged_only: true,
            min_age_days: None,
            include_active_worktrees: false,
        };

        assert_eq!(
            artifact_cleanup_apply_command(&options),
            "homeboy cleanup artifacts --path '/tmp/review scope' --temp-root '/tmp/first root' --temp-root /tmp/second --sort size --limit 7 --merged-only --apply"
        );

        assert_eq!(
            artifact_cleanup_apply_command(&ArtifactCleanupOptions {
                path: None,
                self_artifacts: true,
                ..options
            }),
            "homeboy cleanup artifacts --self --temp-root '/tmp/first root' --temp-root /tmp/second --sort size --limit 7 --merged-only --apply"
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
        write_file(
            &repo.path().join(".gitignore"),
            "target/\nnode_modules/\ndist/\n",
        );
        git(
            repo.path(),
            &["add", "src/lib.rs", "homeboy.json", ".gitignore"],
        );
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
            category: LEGACY_ARTIFACT_CATEGORY.to_string(),
            size_bytes: 1,
            allocated_bytes: 512,
            age_seconds: Some(0),
            liveness: LIVENESS_IDLE.to_string(),
            readiness: READINESS_REBUILD_ON_DEMAND.to_string(),
            rehydrate_command: None,
            source_dirty: false,
            unpushed_commits: false,
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
            &ActiveWorktrees::default(),
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

    /// Install an extension whose only capability is artifact cleanup. Keeps the
    /// fixture focused on the declaration contract under test.
    fn install_artifact_cleanup_extension(id: &str, declarations: serde_json::Value) {
        let mut manifest: homeboy_extension_contract::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Fixture",
                "version": "1.0.0",
                "artifact_cleanup": { "declarations": declarations },
            }))
            .expect("fixture manifest parses");
        manifest.id = id.to_string();
        crate::extension_store::save_manifest(&manifest).expect("save fixture extension");
    }

    /// A declaration for a reinstallable dependency tree resolved beside a
    /// marker file, with rehydration guidance attached.
    fn dependency_tree_declaration(nested: bool) -> serde_json::Value {
        serde_json::json!([{
            "id": "dependency-tree",
            "category": "dependencies",
            "path": "deps",
            "scopes": [{ "manifest_files": ["scope.marker"], "nested": nested }],
            "rehydrate_command": "fixture install",
        }])
    }

    /// Repo whose declared artifact trees are ignored, which is the normal shape
    /// for reconstructable output.
    fn repo_with_ignored_artifacts() -> TempDir {
        let repo = TempDir::new().expect("repo tempdir");
        git(repo.path(), &["init", "-b", "main"]);
        write_file(&repo.path().join("src/lib.rs"), "source");
        write_file(&repo.path().join("scope.marker"), "{}");
        write_file(
            &repo.path().join(".gitignore"),
            "deps/\npackaged/\ntarget/\n",
        );
        git(
            repo.path(),
            &["add", "src/lib.rs", "scope.marker", ".gitignore"],
        );
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

    /// Commit everything Git is willing to track. Declared artifact trees stay
    /// ignored, so this leaves the fixture with no untracked work.
    fn commit_all(repo: &Path) {
        git(repo, &["add", "-A"]);
        git(
            repo,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "fixture",
            ],
        );
    }

    fn register_active_task_worktree(path: &Path) {
        crate::worktree::record_active_for_test("fixture-active", path);
    }

    fn dry_run_options(repo: &Path) -> ArtifactCleanupOptions {
        ArtifactCleanupOptions {
            path: Some(repo.to_path_buf()),
            ..Default::default()
        }
    }

    #[test]
    fn extension_declarations_resolve_beside_supported_install_scopes_only() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(true));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");
            write_file(&repo.path().join("modules/alpha/scope.marker"), "{}");
            write_file(
                &repo.path().join("modules/alpha/deps/index.txt"),
                "installed",
            );
            // No marker here, so this tree is not an install scope the
            // extension supports and must not be resolved.
            write_file(
                &repo.path().join("modules/beta/deps/index.txt"),
                "installed",
            );
            commit_all(repo.path());

            let output = cleanup_artifacts(dry_run_options(repo.path())).expect("dry run");

            let paths: Vec<_> = output
                .candidates
                .iter()
                .map(|row| row.relative_path.clone())
                .collect();
            assert!(paths.contains(&"deps".to_string()));
            assert!(paths.contains(&"modules/alpha/deps".to_string()));
            assert!(!paths.contains(&"modules/beta/deps".to_string()));
            assert!(repo.path().join("deps/package/index.txt").exists());
        });
    }

    #[test]
    fn extension_declarations_report_owner_category_and_rehydration_guidance() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");

            let output = cleanup_artifacts(dry_run_options(repo.path())).expect("dry run");

            let candidate = output
                .candidates
                .iter()
                .find(|row| row.relative_path == "deps")
                .expect("declared dependency tree is a candidate");
            assert_eq!(candidate.declared_by, "extension:fixture");
            assert_eq!(candidate.kind, "dependency-tree");
            assert_eq!(candidate.category, "dependencies");
            assert_eq!(candidate.readiness, READINESS_REHYDRATION_REQUIRED);
            assert_eq!(
                candidate.rehydrate_command.as_deref(),
                Some("fixture install")
            );
            assert!(candidate.age_seconds.is_some());
            assert!(candidate.allocated_bytes > 0);

            let summary = output
                .worktrees
                .first()
                .expect("per-worktree summary is reported");
            assert_eq!(summary.rehydrate_commands, vec!["fixture install"]);
            assert_eq!(
                summary.estimated_allocated_bytes,
                output.estimated_allocated_bytes
            );
            assert_eq!(summary.reclaimed_bytes, 0);
        });
    }

    #[test]
    fn release_assets_are_reported_but_never_removed() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension(
                "fixture",
                serde_json::json!([{
                    "id": "packaged-output",
                    "category": "release_asset",
                    "path": "packaged",
                }]),
            );
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("packaged/bundle.bin"), "deployable");

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output
                .candidates
                .iter()
                .all(|row| row.relative_path != "packaged"));
            let skip = output
                .skipped
                .iter()
                .find(|row| row.relative_path == "packaged")
                .expect("release asset is reported as skipped");
            assert_eq!(skip.category, "release_asset");
            assert_eq!(skip.declared_by, "extension:fixture");
            assert!(skip.reason.contains("release asset"));
            assert!(repo.path().join("packaged/bundle.bin").exists());
        });
    }

    #[test]
    fn age_gate_protects_recently_written_artifacts() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                min_age_days: Some(7),
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output.candidates.is_empty());
            assert!(output
                .skipped
                .iter()
                .any(|row| row.relative_path == "deps" && row.reason.contains("7-day age gate")));
            assert!(repo.path().join("deps/package/index.txt").exists());
        });
    }

    #[test]
    fn declaration_age_floor_composes_with_the_caller_gate() {
        let declaration = ArtifactDeclaration {
            relative_path: "deps".to_string(),
            kind: "dependency-tree".to_string(),
            declared_by: "extension:fixture".to_string(),
            category: "dependencies".to_string(),
            reconstructable: true,
            rehydrate_command: None,
            min_age_days: Some(3),
            liveness_protected: true,
        };

        assert_eq!(
            effective_min_age_days(&ArtifactCleanupOptions::default(), &declaration),
            Some(3)
        );
        assert_eq!(
            effective_min_age_days(
                &ArtifactCleanupOptions {
                    min_age_days: Some(9),
                    ..Default::default()
                },
                &declaration,
            ),
            Some(9),
            "the stricter of the two gates wins"
        );
        assert_eq!(
            effective_min_age_days(
                &ArtifactCleanupOptions {
                    min_age_days: Some(1),
                    ..Default::default()
                },
                &declaration,
            ),
            Some(3),
            "a looser caller gate cannot relax a declared floor"
        );
    }

    #[test]
    fn unreadable_artifact_age_fails_the_gate() {
        assert!(!meets_age_gate(None, 1));
        assert!(!meets_age_gate(Some(SECONDS_PER_DAY - 1), 1));
        assert!(meets_age_gate(Some(SECONDS_PER_DAY), 1));
        assert!(meets_age_gate(Some(0), 0));
    }

    #[test]
    fn active_task_worktrees_protect_extension_declared_artifacts() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");
            write_file(&repo.path().join("target/debug/build.o"), "artifact");
            register_active_task_worktree(repo.path());

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output
                .skipped
                .iter()
                .any(|row| row.relative_path == "deps"
                    && row.reason.contains("active task worktree")));
            assert!(repo.path().join("deps/package/index.txt").exists());
            assert_eq!(
                output.worktrees[0].liveness, LIVENESS_ACTIVE,
                "liveness state is reported for the checkout"
            );
            assert!(
                !repo.path().join("target").exists(),
                "built-in declarations keep their existing behavior on active checkouts"
            );
        });
    }

    #[test]
    fn active_worktree_protection_can_be_opted_out_of() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");
            register_active_task_worktree(repo.path());

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                include_active_worktrees: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output.applied.iter().any(|row| row.relative_path == "deps"));
            assert!(!repo.path().join("deps").exists());
        });
    }

    #[test]
    fn untracked_work_inside_a_declared_artifact_path_is_protected() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = TempDir::new().expect("repo tempdir");
            git(repo.path(), &["init", "-b", "main"]);
            write_file(&repo.path().join("src/lib.rs"), "source");
            write_file(&repo.path().join("scope.marker"), "{}");
            git(repo.path(), &["add", "src/lib.rs", "scope.marker"]);
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
            // Nothing ignores this tree, so Git still considers its contents
            // unaccounted-for work rather than reconstructable output.
            write_file(&repo.path().join("deps/notes.txt"), "operator work");

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output
                .skipped
                .iter()
                .any(|row| row.relative_path == "deps" && row.reason.contains("untracked work")));
            assert!(repo.path().join("deps/notes.txt").exists());
        });
    }

    #[test]
    fn tracked_changes_inside_a_declared_artifact_path_are_protected() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/generated.txt"), "generated");
            git(repo.path(), &["add", "--force", "deps/generated.txt"]);

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output.skipped.iter().any(|row| row.relative_path == "deps"
                && row.reason.contains("tracked or staged source changes")));
            assert!(repo.path().join("deps/generated.txt").exists());
        });
    }

    #[test]
    fn committed_artifact_content_is_protected_even_when_clean() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = TempDir::new().expect("repo tempdir");
            git(repo.path(), &["init", "-b", "main"]);
            write_file(&repo.path().join("src/lib.rs"), "source");
            write_file(&repo.path().join("scope.marker"), "{}");
            // A repository that commits its generated tree on purpose. The
            // working tree is clean, so no status entry reports it.
            write_file(&repo.path().join("deps/index.txt"), "committed output");
            commit_all(repo.path());

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output.candidates.is_empty());
            assert!(output
                .skipped
                .iter()
                .any(|row| row.relative_path == "deps" && row.reason.contains("tracked by Git")));
            assert!(repo.path().join("deps/index.txt").exists());
        });
    }

    #[test]
    fn unmerged_worktrees_protect_extension_declared_artifacts() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");

            let output = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                merged_only: true,
                ..dry_run_options(repo.path())
            })
            .expect("apply");

            assert!(output.candidates.is_empty());
            assert!(output.skipped.iter().any(|row| row.relative_path == "deps"
                && row.reason.contains("not merged into its upstream")));
            assert!(repo.path().join("deps/package/index.txt").exists());
        });
    }

    #[test]
    fn applying_extension_declared_cleanup_twice_is_idempotent() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(&repo.path().join("deps/package/index.txt"), "installed");

            let first = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("first apply");
            let second = cleanup_artifacts(ArtifactCleanupOptions {
                apply: true,
                ..dry_run_options(repo.path())
            })
            .expect("second apply");

            assert_eq!(first.applied_count, 1);
            assert!(first.reclaimed_allocated_bytes > 0);
            assert_eq!(second.candidate_count, 0);
            assert_eq!(second.applied_count, 0);
            assert_eq!(second.failure_count, 0);
            assert_eq!(second.reclaimed_bytes, 0);
            assert!(!repo.path().join("deps").exists());
            assert!(repo.path().join("src/lib.rs").exists());
        });
    }

    #[test]
    fn repository_declarations_win_over_ecosystem_declarations() {
        crate::test_support::with_isolated_home(|_| {
            install_artifact_cleanup_extension("fixture", dependency_tree_declaration(false));
            let repo = repo_with_ignored_artifacts();
            write_file(
                &repo.path().join("homeboy.json"),
                r#"{"artifact_cleanup_paths":["deps"]}"#,
            );
            write_file(&repo.path().join("deps/package/index.txt"), "installed");

            let declarations = artifact_declarations(repo.path()).expect("declarations");

            let deps: Vec<_> = declarations
                .iter()
                .filter(|row| row.relative_path == "deps")
                .collect();
            assert_eq!(deps.len(), 1, "one path resolves to one declaration");
            assert_eq!(deps[0].declared_by, "homeboy.json:artifact_cleanup_paths");
            assert!(!deps[0].liveness_protected);
        });
    }
}
