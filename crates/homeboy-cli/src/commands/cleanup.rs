use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Subcommand, ValueEnum};
use homeboy::core::cleanup::{
    self, ArtifactCleanupOptions, ArtifactCleanupSort, ResourceCleanupOptions,
};
use homeboy::core::controller_runtime::{self, ControllerRuntimeCleanupOptions};
use homeboy::core::defaults;
use homeboy::core::engine;
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::observation::runs_service::{
    self, PersistedArtifactCleanupOptions, RunnerDownloadCleanupOptions,
};
use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::worktree::{self, WorktreeCleanupOptions, WorktreeCleanupOutput};
use homeboy::core::worktree_providers::WorktreeProviderCleanupOptions;
use homeboy::runner::runners::{
    self as runner, RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput,
};
use serde::Serialize;
use serde_json::Value;

use super::runs::{runs_resources, RunsOutput, RunsResourcesArgs, RunsResourcesOutput};
use super::utils::response::{CommandActionableMetadata, CommandNextAction};
use super::CmdResult;

#[derive(Args)]
pub struct CleanupArgs {
    /// Apply cleanup across the selected categories. Omit for inventory dry-run output.
    #[arg(long)]
    pub apply: bool,

    /// Include only these cleanup categories. Comma-separated or repeatable.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub include: Vec<CleanupCategoryArg>,

    /// Exclude these cleanup categories. Comma-separated or repeatable.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub exclude: Vec<CleanupCategoryArg>,

    /// Override the configured terminal-run retention window for this invocation.
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<i64>,

    /// Override the configured maximum number of persisted artifacts inspected.
    #[arg(long, value_name = "N")]
    pub limit: Option<i64>,

    #[command(subcommand)]
    pub command: Option<CleanupCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CleanupCategoryArg {
    RepoArtifacts,
    TaskWorktrees,
    TerminalRuns,
    PersistedRunArtifacts,
    RunnerDownloads,
    RemoteLabWorkspaces,
    RuntimeTmp,
    ControllerScratch,
    ControllerRuntimes,
}

#[derive(Subcommand)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),

    /// Aggregate cleanup across configured external worktree providers
    Worktrees(CleanupWorktreesArgs),
}

#[derive(Args)]
pub struct CleanupArtifactsArgs {
    /// Apply cleanup. Omit for dry-run output.
    #[arg(long)]
    pub apply: bool,

    /// Clean artifacts from the Homeboy source checkout that built this binary.
    #[arg(long = "self", conflicts_with = "path")]
    pub self_artifacts: bool,

    /// Resolve managed worktrees from this checkout instead of the current directory.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Also scan this temp root for detached Homeboy build artifacts. Repeatable.
    #[arg(long, value_name = "PATH")]
    pub temp_root: Vec<PathBuf>,

    /// Sort artifact candidates before reporting or applying cleanup.
    #[arg(long, value_enum, default_value = "discovery")]
    pub sort: CleanupArtifactsSortArg,

    /// Limit artifact candidates reported or removed after sorting.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream. Preserves in-progress cooks' build dirs.
    #[arg(long)]
    pub merged_only: bool,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum CleanupArtifactsSortArg {
    #[default]
    Discovery,
    Size,
}

#[derive(Args)]
pub struct CleanupWorktreesArgs {
    /// Cleanup a specific configured provider. Repeatable.
    #[arg(long = "provider", value_name = "ID", conflicts_with = "all_providers")]
    pub provider: Vec<String>,

    /// Cleanup every enabled configured provider.
    #[arg(long)]
    pub all_providers: bool,

    /// Apply cleanup. Omit for provider preview/dry-run output.
    #[arg(long)]
    pub apply: bool,
}

pub fn run(args: CleanupArgs, _global: &super::GlobalArgs) -> CmdResult<Value> {
    match args.command {
        Some(CleanupCommand::Artifacts(args)) => cleanup::cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: cleanup_intent(args.apply),
                artifacts: Some(ArtifactCleanupOptions {
                    path: args.path,
                    apply: args.apply,
                    self_artifacts: args.self_artifacts,
                    temp_roots: args.temp_root,
                    sort: match args.sort {
                        CleanupArtifactsSortArg::Discovery => ArtifactCleanupSort::Discovery,
                        CleanupArtifactsSortArg::Size => ArtifactCleanupSort::Size,
                    },
                    limit: args.limit,
                    merged_only: args.merged_only,
                }),
                worktree_providers: None,
            },
            defaults::load_config(),
        )
        .and_then(|output| {
            serde_json::to_value(output).map_err(|err| {
                homeboy::core::Error::internal_json(
                    err.to_string(),
                    Some("serialize cleanup artifacts output".to_string()),
                )
            })
        })
        .map(|output| (output, 0)),
        Some(CleanupCommand::Worktrees(args)) => cleanup::cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: cleanup_intent(args.apply),
                artifacts: None,
                worktree_providers: Some(WorktreeProviderCleanupOptions {
                    provider: args.provider,
                    all_providers: args.all_providers,
                    apply: args.apply,
                }),
            },
            defaults::load_config(),
        )
        .and_then(|output| {
            serde_json::to_value(output).map_err(|err| {
                homeboy::core::Error::internal_json(
                    err.to_string(),
                    Some("serialize cleanup worktrees output".to_string()),
                )
            })
        })
        .map(|output| (output, 0)),
        None => cleanup_inventory(args).map(|output| (output, 0)),
    }
}

#[derive(Debug, Serialize)]
pub struct CleanupInventoryOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub category_count: usize,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub retention: CleanupRetentionManifest,
    pub categories: Vec<CleanupInventoryCategory>,
    #[serde(rename = "_homeboy_actionable")]
    pub actionable: CommandActionableMetadata,
}

/// Stable, serialized policy snapshot for a cleanup plan or apply result.
#[derive(Debug, Serialize)]
pub struct CleanupRetentionManifest {
    pub schema: &'static str,
    pub terminal_run_days: i64,
    pub runtime_tmp_days: u64,
    pub controller_runtime_days: u64,
    pub controller_runtime_max_bytes: u64,
    pub limit: i64,
    pub terminal_run_guard: bool,
}

#[derive(Debug, Serialize)]
pub struct CleanupInventoryCategory {
    pub category: &'static str,
    pub canonical_cleanup_command: String,
    pub specialist_command: String,
    pub included: bool,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub output: Value,
}

#[derive(Clone, Copy)]
pub(crate) struct CleanupInventoryCategoryMetadata {
    pub(crate) category: &'static str,
    pub(crate) include_arg: &'static str,
    pub(crate) dry_run_command: &'static str,
    pub(crate) apply_command: &'static str,
}

impl CleanupInventoryCategoryMetadata {
    pub(crate) fn specialist_command(self, apply: bool) -> &'static str {
        if apply {
            self.apply_command
        } else {
            self.dry_run_command
        }
    }

    pub(crate) fn canonical_cleanup_command(self, apply: bool) -> String {
        let command = format!("homeboy cleanup --include {}", self.include_arg);
        if apply {
            format!("{command} --apply")
        } else {
            command
        }
    }
}

const REPO_ARTIFACTS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "repo_artifacts",
        include_arg: "repo-artifacts",
        dry_run_command: "homeboy cleanup artifacts",
        apply_command: "homeboy cleanup artifacts --apply",
    };

const TASK_WORKTREES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "task_worktrees",
        include_arg: "task-worktrees",
        dry_run_command: "homeboy worktree cleanup --dry-run --cleanup-branches",
        apply_command: "homeboy worktree cleanup --cleanup-branches",
    };

const PERSISTED_RUN_ARTIFACTS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "persisted_run_artifacts",
        include_arg: "persisted-run-artifacts",
        dry_run_command: "homeboy runs artifact cleanup-persisted",
        apply_command: "homeboy runs artifact cleanup-persisted --apply",
    };

const TERMINAL_RUNS_METADATA: CleanupInventoryCategoryMetadata = CleanupInventoryCategoryMetadata {
    category: "terminal_runs",
    include_arg: "terminal-runs",
    dry_run_command: "homeboy runs retention",
    apply_command: "homeboy runs retention --apply",
};

pub(crate) const RUNNER_DOWNLOADS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "runner_downloads",
        include_arg: "runner-downloads",
        dry_run_command: "homeboy runs artifact cleanup-downloads",
        apply_command: "homeboy runs artifact cleanup-downloads --apply",
    };

const RUNTIME_TMP_METADATA: CleanupInventoryCategoryMetadata = CleanupInventoryCategoryMetadata {
    category: "runtime_tmp",
    include_arg: "runtime-tmp",
    dry_run_command: "homeboy self cleanup-runtime-tmp",
    apply_command: "homeboy self cleanup-runtime-tmp --apply",
};

const REMOTE_LAB_WORKSPACES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "remote_lab_workspaces",
        include_arg: "remote-lab-workspaces",
        dry_run_command: "homeboy runner workspace prune <runner>",
        apply_command: "homeboy runner workspace prune <runner> --apply --passes 10",
    };

const CONTROLLER_SCRATCH_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "controller_scratch",
        include_arg: "controller-scratch",
        dry_run_command: "homeboy cleanup --include controller-scratch",
        apply_command: "homeboy cleanup --include controller-scratch --apply",
    };

const CONTROLLER_RUNTIMES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "controller_runtimes",
        include_arg: "controller-runtimes",
        dry_run_command: "homeboy cleanup --include controller-runtimes",
        apply_command: "homeboy cleanup --include controller-runtimes --apply",
    };

fn cleanup_inventory(args: CleanupArgs) -> homeboy::core::Result<Value> {
    let selected = CleanupCategorySelection::new(args.include, args.exclude);
    let apply = args.apply;
    let configured = defaults::load_config().retention;
    let terminal_run_days = args.older_than_days.unwrap_or(configured.terminal_run_days);
    let limit = args.limit.unwrap_or(configured.limit);
    if terminal_run_days < 0 || limit < 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "retention",
            "--older-than-days must be zero or greater and --limit must be positive",
            None,
            None,
        ));
    }
    let mut categories = Vec::new();

    if selected.skips_repo_artifacts_without_checkout(current_dir_is_git_checkout()) {
        categories.push(repo_artifacts_checkout_skipped_category(apply));
    } else if selected.includes(CleanupCategoryArg::RepoArtifacts) {
        let output = cleanup::cleanup_artifacts(ArtifactCleanupOptions {
            path: None,
            apply,
            self_artifacts: false,
            temp_roots: Vec::new(),
            sort: ArtifactCleanupSort::Discovery,
            limit: None,
            merged_only: false,
        })?;
        categories.push(category_from_output(
            REPO_ARTIFACTS_METADATA,
            apply,
            output.candidate_count,
            output.applied_count,
            output.skipped_count,
            output.estimated_bytes,
            output.reclaimed_bytes,
            output,
        )?);
    }

    if selected.includes(CleanupCategoryArg::TaskWorktrees) {
        let output = worktree::cleanup(WorktreeCleanupOptions {
            force: false,
            dry_run: !apply,
            cleanup_branches: apply,
            allow_unmerged_branches: false,
        })?;
        categories.push(task_worktrees_category(output, apply)?);
    }

    if selected.includes(CleanupCategoryArg::TerminalRuns) {
        let output =
            runs_service::retain_terminal_runs(runs_service::TerminalRunRetentionOptions {
                apply,
                older_than_days: terminal_run_days,
                limit,
            })?;
        let lifecycle_bytes = output
            .lifecycle_directories
            .iter()
            .map(|directory| directory.size_bytes)
            .sum();
        categories.push(category_from_output(
            TERMINAL_RUNS_METADATA,
            apply,
            output.candidate_run_ids.len(),
            output.removed_run_count,
            output.skipped_run_ids.len(),
            lifecycle_bytes,
            if apply { lifecycle_bytes } else { 0 },
            output,
        )?);
    }

    if selected.includes(CleanupCategoryArg::PersistedRunArtifacts) {
        let persisted =
            runs_service::cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
                apply,
                older_than_days: terminal_run_days,
                run_id: None,
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit,
                terminal_only: true,
            })?;
        let resources = runs_resources(RunsResourcesArgs {
            cleanup_plan: true,
            apply: false,
            cleanup_root: None,
            limit: 1000,
            ..RunsResourcesArgs::default()
        })?
        .0;
        let RunsOutput::Resources(resources) = resources else {
            return Err(homeboy::core::Error::internal_unexpected(
                "runs resources returned unexpected output",
            ));
        };
        categories.push(persisted_artifacts_category(persisted, resources, apply)?);
    }

    if selected.includes(CleanupCategoryArg::RunnerDownloads) {
        let output = runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
            apply,
            runner: None,
            run_id: None,
        })?;
        categories.push(category_from_output(
            RUNNER_DOWNLOADS_METADATA,
            apply,
            output.file_count + output.directory_count,
            usize::from(output.removed),
            0,
            output.size_bytes,
            if output.removed { output.size_bytes } else { 0 },
            output,
        )?);
    }

    if selected.includes(CleanupCategoryArg::RemoteLabWorkspaces) {
        categories.extend(remote_lab_workspace_categories(apply)?);
    }

    if selected.includes(CleanupCategoryArg::RuntimeTmp) {
        let output = engine::temp::cleanup_runtime_tmp(
            apply,
            configured.runtime_tmp_days,
            None,
            usize::try_from(limit).unwrap_or(usize::MAX),
        )?;
        categories.push(category_from_output(
            RUNTIME_TMP_METADATA,
            apply,
            output.planned_count,
            output.removed_count,
            output.skipped_count,
            output.totals.planned_size_bytes,
            output.totals.removed_size_bytes,
            output,
        )?);
    }

    if selected.includes(CleanupCategoryArg::ControllerScratch) {
        let output = homeboy::agents::controller_scratch::cleanup(
            homeboy::agents::controller_scratch::ControllerScratchCleanupOptions {
                apply,
                limit: usize::try_from(limit).unwrap_or(usize::MAX),
            },
        )?;
        categories.push(category_from_output(
            CONTROLLER_SCRATCH_METADATA,
            apply,
            output.candidate_count,
            output.applied_count,
            output.skipped_count,
            output.estimated_bytes,
            output.reclaimed_bytes,
            output,
        )?);
    }
    if selected.includes(CleanupCategoryArg::ControllerRuntimes) {
        let output = controller_runtime::cleanup(ControllerRuntimeCleanupOptions {
            apply,
            min_age: std::time::Duration::from_secs(
                configured.controller_runtime_days.saturating_mul(86_400),
            ),
            max_total_bytes: configured.controller_runtime_max_bytes,
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        })?;
        let estimated_bytes = output
            .snapshots
            .iter()
            .filter(|snapshot| snapshot.eligible)
            .map(|snapshot| snapshot.size_bytes)
            .sum();
        let reclaimed_bytes = output
            .removed
            .iter()
            .filter_map(|path| {
                output
                    .snapshots
                    .iter()
                    .find(|snapshot| snapshot.pins.contains(path))
                    .map(|snapshot| snapshot.size_bytes)
            })
            .sum();
        categories.push(category_from_output(
            CONTROLLER_RUNTIMES_METADATA,
            apply,
            output.eligible.len(),
            output.removed.len(),
            output.retained.len(),
            estimated_bytes,
            reclaimed_bytes,
            output,
        )?);
    }

    let candidate_count = categories
        .iter()
        .map(|category| category.candidate_count)
        .sum();
    let applied_count = categories
        .iter()
        .map(|category| category.applied_count)
        .sum();
    let skipped_count = categories
        .iter()
        .map(|category| category.skipped_count)
        .sum();
    let estimated_bytes = categories
        .iter()
        .map(|category| category.estimated_bytes)
        .sum();
    let reclaimed_bytes = categories
        .iter()
        .map(|category| category.reclaimed_bytes)
        .sum();
    let actionable = cleanup_actionable(&categories, apply);
    serde_json::to_value(CleanupInventoryOutput {
        command: "cleanup.inventory",
        mode: if apply { "apply" } else { "dry_run" },
        category_count: categories.len(),
        candidate_count,
        applied_count,
        skipped_count,
        estimated_bytes,
        reclaimed_bytes,
        retention: CleanupRetentionManifest {
            schema: "homeboy/retention-manifest/v1",
            terminal_run_days,
            runtime_tmp_days: configured.runtime_tmp_days,
            controller_runtime_days: configured.controller_runtime_days,
            controller_runtime_max_bytes: configured.controller_runtime_max_bytes,
            limit,
            terminal_run_guard: true,
        },
        categories,
        actionable,
    })
    .map_err(|err| {
        homeboy::core::Error::internal_json(err.to_string(), Some("cleanup inventory".to_string()))
    })
}

struct CleanupCategorySelection {
    include: Vec<CleanupCategoryArg>,
    exclude: Vec<CleanupCategoryArg>,
}

impl CleanupCategorySelection {
    fn new(include: Vec<CleanupCategoryArg>, exclude: Vec<CleanupCategoryArg>) -> Self {
        Self { include, exclude }
    }

    fn includes(&self, category: CleanupCategoryArg) -> bool {
        (self.include.is_empty() || self.include.contains(&category))
            && !self.exclude.contains(&category)
    }

    fn skips_repo_artifacts_without_checkout(&self, in_git_checkout: bool) -> bool {
        !in_git_checkout
            && self.include.is_empty()
            && !self.exclude.contains(&CleanupCategoryArg::RepoArtifacts)
    }
}

fn current_dir_is_git_checkout() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(Path::new("."))
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.trim_ascii() == b"true")
}

fn repo_artifacts_checkout_skipped_category(apply: bool) -> CleanupInventoryCategory {
    CleanupInventoryCategory {
        category: REPO_ARTIFACTS_METADATA.category,
        canonical_cleanup_command: REPO_ARTIFACTS_METADATA.canonical_cleanup_command(apply),
        specialist_command: "homeboy cleanup artifacts --path <PATH>".to_string(),
        included: true,
        skipped: true,
        skip_reason: Some(
            "current directory is not inside a git checkout; run from a checkout or use `homeboy cleanup artifacts --path <PATH>`".to_string(),
        ),
        candidate_count: 0,
        applied_count: 0,
        skipped_count: 1,
        estimated_bytes: 0,
        reclaimed_bytes: 0,
        output: serde_json::json!({ "path_remediation": "homeboy cleanup artifacts --path <PATH>" }),
    }
}

fn category_from_output<T: Serialize>(
    metadata: CleanupInventoryCategoryMetadata,
    apply: bool,
    candidate_count: usize,
    applied_count: usize,
    skipped_count: usize,
    estimated_bytes: u64,
    reclaimed_bytes: u64,
    output: T,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    category_from_command(
        metadata.category,
        metadata.canonical_cleanup_command(apply),
        metadata.specialist_command(apply).to_string(),
        candidate_count,
        applied_count,
        skipped_count,
        estimated_bytes,
        reclaimed_bytes,
        output,
    )
}

fn category_from_command<T: Serialize>(
    category: &'static str,
    canonical_cleanup_command: String,
    specialist_command: String,
    candidate_count: usize,
    applied_count: usize,
    skipped_count: usize,
    estimated_bytes: u64,
    reclaimed_bytes: u64,
    output: T,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    Ok(CleanupInventoryCategory {
        category,
        canonical_cleanup_command,
        specialist_command,
        included: true,
        skipped: false,
        skip_reason: None,
        candidate_count,
        applied_count,
        skipped_count,
        estimated_bytes,
        reclaimed_bytes,
        output: serde_json::to_value(output).map_err(|err| {
            homeboy::core::Error::internal_json(err.to_string(), Some(category.to_string()))
        })?,
    })
}

fn task_worktrees_category(
    output: WorktreeCleanupOutput,
    apply: bool,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    category_from_output(
        TASK_WORKTREES_METADATA,
        apply,
        output.counts.candidates,
        output.counts.removed + output.counts.branches_deleted,
        output.counts.skipped,
        0,
        0,
        output,
    )
}

fn persisted_artifacts_category(
    persisted: runs_service::PersistedArtifactCleanupOutcome,
    resources: RunsResourcesOutput,
    apply: bool,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    let resource_cleanup_candidates = resources
        .cleanup
        .as_ref()
        .map(|cleanup| cleanup.candidate_count)
        .unwrap_or(0);
    let output = serde_json::json!({
        "persisted_artifacts": persisted,
        "resource_lifecycle": resources,
    });
    Ok(CleanupInventoryCategory {
        category: PERSISTED_RUN_ARTIFACTS_METADATA.category,
        canonical_cleanup_command: PERSISTED_RUN_ARTIFACTS_METADATA
            .canonical_cleanup_command(apply),
        specialist_command: PERSISTED_RUN_ARTIFACTS_METADATA
            .specialist_command(apply)
            .to_string(),
        included: true,
        skipped: false,
        skip_reason: None,
        candidate_count: persisted.planned_record_count + resource_cleanup_candidates,
        applied_count: persisted.removed_record_count,
        skipped_count: persisted.skipped_count,
        estimated_bytes: persisted.totals.planned_size_bytes,
        reclaimed_bytes: persisted.totals.removed_size_bytes,
        output,
    })
}

fn remote_lab_workspace_categories(
    apply: bool,
) -> homeboy::core::Result<Vec<CleanupInventoryCategory>> {
    let mut categories = Vec::new();
    for status in runner::statuses()? {
        if !remote_workspace_cleanup_connected(&status) {
            categories.push(CleanupInventoryCategory {
                category: "remote_lab_workspaces",
                canonical_cleanup_command: REMOTE_LAB_WORKSPACES_METADATA
                    .canonical_cleanup_command(apply),
                specialist_command: format!(
                    "homeboy runner workspace prune {}",
                    quote_arg(&status.runner_id)
                ),
                included: true,
                skipped: true,
                skip_reason: Some("runner is not connected".to_string()),
                candidate_count: 0,
                applied_count: 0,
                skipped_count: 1,
                estimated_bytes: 0,
                reclaimed_bytes: 0,
                output: serde_json::json!({ "runner_id": status.runner_id, "connected": status.connected }),
            });
            continue;
        }
        let output = match runner::prune_workspaces(
            &status.runner_id,
            RunnerWorkspacePruneOptions {
                apply,
                min_age_hours: 24,
                limit: 25,
                passes: if apply { 10 } else { 1 },
            },
        ) {
            Ok((output, _)) => output,
            Err(error) => {
                categories.push(CleanupInventoryCategory {
                    category: "remote_lab_workspaces",
                    canonical_cleanup_command: REMOTE_LAB_WORKSPACES_METADATA
                        .canonical_cleanup_command(apply),
                    specialist_command: format!(
                        "homeboy runner workspace prune {}",
                        quote_arg(&status.runner_id)
                    ),
                    included: true,
                    skipped: true,
                    skip_reason: Some(error.message),
                    candidate_count: 0,
                    applied_count: 0,
                    skipped_count: 1,
                    estimated_bytes: 0,
                    reclaimed_bytes: 0,
                    output: serde_json::json!({ "runner_id": status.runner_id }),
                });
                continue;
            }
        };
        categories.push(remote_workspace_category(output, apply)?);
    }
    Ok(categories)
}

fn remote_workspace_cleanup_connected(status: &runner::RunnerStatusReport) -> bool {
    status.runner_id != "local" && status.is_connected()
}

fn remote_workspace_category(
    output: RunnerWorkspacePruneOutput,
    apply: bool,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    let command = if apply {
        format!(
            "homeboy runner workspace prune {} --apply --passes 10",
            quote_arg(&output.runner_id)
        )
    } else {
        format!(
            "homeboy runner workspace prune {}",
            quote_arg(&output.runner_id)
        )
    };
    category_from_command(
        "remote_lab_workspaces",
        REMOTE_LAB_WORKSPACES_METADATA.canonical_cleanup_command(apply),
        command,
        output.total_candidate_count,
        output.removed.len(),
        output.skipped.len(),
        output.total_candidate_bytes,
        output.total_removed_bytes,
        output,
    )
}

fn cleanup_actionable(
    categories: &[CleanupInventoryCategory],
    apply: bool,
) -> CommandActionableMetadata {
    let mut actionable = CommandActionableMetadata::default();
    for category in categories {
        if category.skipped || category.candidate_count == 0 {
            continue;
        }
        actionable.next_actions.push(CommandNextAction::new(
            format!("{} cleanup", category.category.replace('_', " ")),
            if apply {
                category.specialist_command.clone()
            } else {
                apply_command(&category.specialist_command)
            },
        ));
    }
    actionable
}

fn apply_command(command: &str) -> String {
    if command.contains(" --apply") {
        command.to_string()
    } else {
        format!("{command} --apply")
    }
}

fn cleanup_intent(apply: bool) -> ResourceCleanupIntent {
    if apply {
        ResourceCleanupIntent::Apply
    } else {
        ResourceCleanupIntent::DryRun
    }
}

pub(crate) fn render_artifact_cleanup_summary(payload: &Value) -> Option<String> {
    let payload = if payload.get("command").and_then(Value::as_str)? == "cleanup.resources" {
        payload.get("artifacts")?
    } else {
        payload
    };

    if payload.get("command").and_then(Value::as_str)? != "cleanup.artifacts" {
        return None;
    }

    let mode = payload.get("mode").and_then(Value::as_str)?;
    let root = payload.get("root").and_then(Value::as_str).unwrap_or(".");
    let candidate_count = payload
        .get("candidate_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let applied_count = payload
        .get("applied_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let skipped_count = payload
        .get("skipped_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let estimated_bytes = payload
        .get("estimated_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reclaimed_bytes = payload
        .get("reclaimed_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let remaining_candidates = candidate_count.saturating_sub(applied_count);

    let mut lines = vec![
        "Artifact cleanup summary".to_string(),
        format!(
            "Mode: {}",
            if mode == "apply" { "apply" } else { "dry run" }
        ),
        format!("Root: {root}"),
        format!("Candidates: {candidate_count}"),
        format!("Applied: {applied_count}"),
        format!("Remaining candidates: {remaining_candidates}"),
        format!("Estimated reclaimable: {}", format_bytes(estimated_bytes)),
        format!("Reclaimed: {}", format_bytes(reclaimed_bytes)),
        format!("Skipped: {skipped_count}"),
    ];

    for (reason, count) in skipped_counts_by_reason(payload) {
        lines.push(format!("  - {reason}: {count}"));
    }

    let candidate_display_limit = 10;
    let candidate_lines = artifact_candidate_lines(payload, candidate_display_limit);
    if !candidate_lines.is_empty() {
        lines.push(format!(
            "Rebuildable artifacts (showing {} of {candidate_count}):",
            candidate_lines.len()
        ));
        lines.extend(candidate_lines);
        if candidate_count > candidate_display_limit as u64 {
            lines.push(format!(
                "Full candidate list is available in JSON output; use --sort size --limit {candidate_display_limit} for a bounded largest-first review."
            ));
        }
    }

    let next = if mode == "apply" {
        format!("homeboy cleanup artifacts --path {}", quote_arg(root))
    } else {
        format!(
            "homeboy cleanup artifacts --path {} --apply",
            quote_arg(root)
        )
    };
    lines.push(format!("Next safe command: {next}"));
    lines.push(String::new());

    Some(lines.join("\n"))
}

pub(crate) fn render_cleanup_summary(payload: &Value) -> Option<String> {
    render_artifact_cleanup_summary(payload).or_else(|| render_worktree_cleanup_summary(payload))
}

pub(crate) fn render_worktree_cleanup_summary(payload: &Value) -> Option<String> {
    let payload = if payload.get("command").and_then(Value::as_str)? == "cleanup.resources" {
        payload.get("worktree_providers")?
    } else {
        payload
    };

    if payload.get("command").and_then(Value::as_str)? != "cleanup.worktrees" {
        return None;
    }

    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("preview");
    let provider_count = payload
        .get("provider_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let success_count = payload
        .get("success_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let failure_count = payload
        .get("failure_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut lines = vec![
        "Worktree provider cleanup summary".to_string(),
        format!(
            "Mode: {}",
            if mode == "apply" { "apply" } else { "preview" }
        ),
        format!("Providers: {provider_count}"),
        format!("Succeeded: {success_count}"),
        format!("Failed: {failure_count}"),
    ];

    if let Some(providers) = payload.get("providers").and_then(Value::as_array) {
        for provider in providers {
            let provider_id = provider
                .get("provider_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let success = provider
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            lines.push(format!(
                "Provider {provider_id}: {}",
                if success { "ok" } else { "failed" }
            ));
            if let Some(command) = provider_command(provider) {
                lines.push(format!("  Command: {command}"));
            }
            if let Some(phase) = provider.get("phase").and_then(Value::as_str) {
                lines.push(format!("  Phase: {phase}"));
            }
            if let Some(progress) = provider.get("last_progress").and_then(Value::as_str) {
                lines.push(format!("  Last observed progress: {progress}"));
            }
            if let Some(run_refs) = provider.get("run_refs").and_then(Value::as_array) {
                for run_ref in run_refs {
                    if let Some(run_id) = run_ref.get("run_id").and_then(Value::as_str) {
                        lines.push(format!("  Run: {run_id}"));
                    }
                    if let Some(status_command) =
                        run_ref.get("status_command").and_then(Value::as_str)
                    {
                        lines.push(format!("  Status command: {status_command}"));
                    }
                }
            }
            if let Some(follow_up) = provider.get("follow_up_command").and_then(Value::as_str) {
                lines.push(format!("  Safe follow-up command: {follow_up}"));
            }
            if let Some(error) = provider.get("error").and_then(Value::as_str) {
                lines.push(format!("  Error: {error}"));
            }
        }
    }

    lines.push(String::new());
    Some(lines.join("\n"))
}

fn provider_command(provider: &Value) -> Option<String> {
    let argv = provider.get("command_run")?.as_array()?;
    let parts: Vec<String> = argv
        .iter()
        .filter_map(Value::as_str)
        .map(quote_arg)
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn artifact_candidate_lines(payload: &Value, limit: usize) -> Vec<String> {
    payload
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
        .filter_map(|row| {
            let path = row.get("path").and_then(Value::as_str)?;
            let bytes = row.get("size_bytes").and_then(Value::as_u64).unwrap_or(0);
            Some(format!("  - {} {}", format_bytes(bytes), path))
        })
        .collect()
}

fn skipped_counts_by_reason(payload: &Value) -> Vec<(String, u64)> {
    let mut counts = std::collections::BTreeMap::new();
    if let Some(skipped) = payload.get("skipped").and_then(Value::as_array) {
        for row in skipped {
            if let Some(reason) = row.get("reason").and_then(Value::as_str) {
                *counts.entry(reason.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts.into_iter().collect()
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    match bytes {
        0..=1023 => format!("{bytes} B"),
        _ if (bytes as f64) < MIB => format!("{:.1} KiB", bytes as f64 / KIB),
        _ if (bytes as f64) < GIB => format!("{:.1} MiB", bytes as f64 / MIB),
        _ => format!("{:.1} GiB", bytes as f64 / GIB),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use homeboy::runner::runners::{RunnerActiveJobState, RunnerSessionState, RunnerStatusReport};
    use serde_json::json;

    use crate::cli_surface::{Cli, Commands};

    use super::*;

    #[test]
    fn cleanup_artifacts_cli_accepts_bounded_review_flags() {
        let cli = Cli::parse_from([
            "homeboy",
            "cleanup",
            "artifacts",
            "--sort",
            "size",
            "--limit",
            "7",
            "--merged-only",
        ]);

        let Commands::Cleanup(args) = cli.command else {
            panic!("expected cleanup command");
        };
        let Some(CleanupCommand::Artifacts(args)) = args.command else {
            panic!("expected cleanup artifacts command");
        };
        assert!(matches!(args.sort, CleanupArtifactsSortArg::Size));
        assert_eq!(args.limit, Some(7));
        assert!(args.merged_only);
    }

    #[test]
    fn cleanup_front_door_accepts_include_exclude_without_subcommand() {
        let cli = Cli::parse_from([
            "homeboy",
            "cleanup",
            "--include",
            "repo-artifacts,task-worktrees",
            "--exclude",
            "runtime-tmp",
            "--apply",
        ]);

        let Commands::Cleanup(args) = cli.command else {
            panic!("expected cleanup command");
        };
        assert!(args.apply);
        assert!(args.command.is_none());
        assert_eq!(args.include.len(), 2);
        assert!(args.include.contains(&CleanupCategoryArg::RepoArtifacts));
        assert!(args.include.contains(&CleanupCategoryArg::TaskWorktrees));
        assert_eq!(args.exclude, vec![CleanupCategoryArg::RuntimeTmp]);
    }

    #[test]
    fn cleanup_category_selection_is_table_driven() {
        let cases = [
            (vec![], vec![], CleanupCategoryArg::RepoArtifacts, true),
            (
                vec![CleanupCategoryArg::TaskWorktrees],
                vec![],
                CleanupCategoryArg::RepoArtifacts,
                false,
            ),
            (
                vec![CleanupCategoryArg::TaskWorktrees],
                vec![],
                CleanupCategoryArg::TaskWorktrees,
                true,
            ),
            (
                vec![],
                vec![CleanupCategoryArg::RuntimeTmp],
                CleanupCategoryArg::RuntimeTmp,
                false,
            ),
        ];

        for (include, exclude, category, expected) in cases {
            assert_eq!(
                CleanupCategorySelection::new(include, exclude).includes(category),
                expected
            );
        }
    }

    #[test]
    fn repo_artifacts_checkout_precondition_is_default_only() {
        let cases = [
            ("default", vec![], vec![], true, true),
            (
                "explicit include remains strict",
                vec![CleanupCategoryArg::RepoArtifacts],
                vec![],
                true,
                false,
            ),
            (
                "exclude remains excluded",
                vec![],
                vec![CleanupCategoryArg::RepoArtifacts],
                false,
                false,
            ),
        ];

        for (name, include, exclude, included, skipped) in cases {
            let selection = CleanupCategorySelection::new(include, exclude);
            assert_eq!(
                selection.includes(CleanupCategoryArg::RepoArtifacts),
                included,
                "{name}: selection"
            );
            assert_eq!(
                selection.skips_repo_artifacts_without_checkout(false),
                skipped,
                "{name}: checkout precondition"
            );
        }
    }

    #[test]
    fn repo_artifacts_checkout_skip_is_structured_with_path_remediation() {
        let category = repo_artifacts_checkout_skipped_category(false);

        assert_eq!(category.category, "repo_artifacts");
        assert!(category.included);
        assert!(category.skipped);
        assert_eq!(category.skipped_count, 1);
        assert_eq!(
            category.skip_reason.as_deref(),
            Some(
                "current directory is not inside a git checkout; run from a checkout or use `homeboy cleanup artifacts --path <PATH>`"
            )
        );
        assert_eq!(
            category.output["path_remediation"],
            "homeboy cleanup artifacts --path <PATH>"
        );
    }

    #[test]
    fn cleanup_inventory_static_metadata_preserves_specialist_commands() {
        let cases = [
            (
                REPO_ARTIFACTS_METADATA,
                "repo_artifacts",
                "repo-artifacts",
                "homeboy cleanup artifacts",
                "homeboy cleanup artifacts --apply",
            ),
            (
                TASK_WORKTREES_METADATA,
                "task_worktrees",
                "task-worktrees",
                "homeboy worktree cleanup --dry-run --cleanup-branches",
                "homeboy worktree cleanup --cleanup-branches",
            ),
            (
                TERMINAL_RUNS_METADATA,
                "terminal_runs",
                "terminal-runs",
                "homeboy runs retention",
                "homeboy runs retention --apply",
            ),
            (
                PERSISTED_RUN_ARTIFACTS_METADATA,
                "persisted_run_artifacts",
                "persisted-run-artifacts",
                "homeboy runs artifact cleanup-persisted",
                "homeboy runs artifact cleanup-persisted --apply",
            ),
            (
                RUNNER_DOWNLOADS_METADATA,
                "runner_downloads",
                "runner-downloads",
                "homeboy runs artifact cleanup-downloads",
                "homeboy runs artifact cleanup-downloads --apply",
            ),
            (
                RUNTIME_TMP_METADATA,
                "runtime_tmp",
                "runtime-tmp",
                "homeboy self cleanup-runtime-tmp",
                "homeboy self cleanup-runtime-tmp --apply",
            ),
            (
                CONTROLLER_RUNTIMES_METADATA,
                "controller_runtimes",
                "controller-runtimes",
                "homeboy cleanup --include controller-runtimes",
                "homeboy cleanup --include controller-runtimes --apply",
            ),
        ];

        for (metadata, category, include_arg, dry_run_command, apply_command) in cases {
            assert_eq!(metadata.category, category);
            assert_eq!(metadata.include_arg, include_arg);
            assert_eq!(metadata.specialist_command(false), dry_run_command);
            assert_eq!(metadata.specialist_command(true), apply_command);
            assert_eq!(
                metadata.canonical_cleanup_command(false),
                format!("homeboy cleanup --include {include_arg}")
            );
            assert_eq!(
                metadata.canonical_cleanup_command(true),
                format!("homeboy cleanup --include {include_arg} --apply")
            );
        }
    }

    #[test]
    fn remote_workspace_cleanup_uses_authoritative_runner_session_state() {
        let cases = [
            (RunnerSessionState::Connected, false, true),
            (RunnerSessionState::Disconnected, true, false),
            (RunnerSessionState::Recorded, true, false),
        ];

        for (state, connected, expected) in cases {
            let report = RunnerStatusReport {
                runner_id: "lab".to_string(),
                connected,
                state,
                session: None,
                stale_daemon: None,
                daemon_freshness: None,
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                stale_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                active_job_recovery_evidence: None,
                session_path: "/tmp/lab.json".to_string(),
            };

            assert_eq!(
                remote_workspace_cleanup_connected(&report),
                expected,
                "state={:?}",
                report.state
            );
        }
    }

    #[test]
    fn cleanup_artifacts_summary_emphasizes_operator_counts() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/homeboy repo",
            "worktree_count": 2,
            "candidate_count": 3,
            "skipped_count": 2,
            "applied_count": 0,
            "estimated_bytes": 1572864,
            "reclaimed_bytes": 0,
            "candidates": [],
            "skipped": [
                { "reason": "artifact path contains tracked or staged source changes" },
                { "reason": "artifact path contains tracked or staged source changes" }
            ],
            "applied": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Artifact cleanup summary\n"));
        assert!(summary.contains("Candidates: 3\n"));
        assert!(summary.contains("Applied: 0\n"));
        assert!(summary.contains("Remaining candidates: 3\n"));
        assert!(summary.contains("Estimated reclaimable: 1.5 MiB\n"));
        assert!(summary.contains("Reclaimed: 0 B\n"));
        assert!(
            summary.contains("  - artifact path contains tracked or staged source changes: 2\n")
        );
        assert!(summary.contains(
            "Next safe command: homeboy cleanup artifacts --path '/tmp/homeboy repo' --apply\n"
        ));
    }

    #[test]
    fn cleanup_artifacts_apply_summary_reports_remaining_after_applied() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "apply",
            "root": "/tmp/homeboy",
            "candidate_count": 4,
            "skipped_count": 1,
            "applied_count": 3,
            "estimated_bytes": 4096,
            "reclaimed_bytes": 3072,
            "skipped": [
                { "reason": "worktree branch is not merged into its upstream" }
            ]
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Mode: apply\n"));
        assert!(summary.contains("Remaining candidates: 1\n"));
        assert!(summary.contains("Reclaimed: 3.0 KiB\n"));
        assert!(
            summary.contains("Next safe command: homeboy cleanup artifacts --path /tmp/homeboy\n")
        );
    }

    #[test]
    fn cleanup_artifacts_summary_lists_candidates_in_payload_order() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/repo",
            "candidate_count": 2,
            "skipped_count": 0,
            "applied_count": 0,
            "estimated_bytes": 3072,
            "reclaimed_bytes": 0,
            "candidates": [
                { "path": "/tmp/repo/node_modules", "size_bytes": 2048 },
                { "path": "/tmp/repo/dist", "size_bytes": 1024 }
            ],
            "skipped": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Rebuildable artifacts (showing 2 of 2):"));
        let first = summary.find("  - 2.0 KiB /tmp/repo/node_modules").unwrap();
        let second = summary.find("  - 1.0 KiB /tmp/repo/dist").unwrap();
        assert!(first < second);
    }

    #[test]
    fn cleanup_artifacts_summary_marks_truncated_candidate_list() {
        let candidates: Vec<_> = (0..12)
            .map(|index| {
                json!({
                    "path": format!("/tmp/repo/target-{index}"),
                    "size_bytes": 1024
                })
            })
            .collect();
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/repo",
            "candidate_count": 12,
            "skipped_count": 0,
            "applied_count": 0,
            "estimated_bytes": 12288,
            "reclaimed_bytes": 0,
            "candidates": candidates,
            "skipped": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Rebuildable artifacts (showing 10 of 12):"));
        assert!(summary.contains("Full candidate list is available in JSON output"));
        assert!(summary.contains("--sort size --limit 10"));
        assert!(!summary.contains("/tmp/repo/target-10"));
    }

    #[test]
    fn cleanup_worktrees_summary_surfaces_provider_progress_and_refs() {
        let payload = json!({
            "command": "cleanup.resources",
            "mode": "apply",
            "worktree_providers": {
                "command": "cleanup.worktrees",
                "mode": "apply",
                "provider_count": 1,
                "success_count": 1,
                "failure_count": 0,
                "providers": [
                    {
                        "provider_id": "fixture",
                        "success": true,
                        "mode": "apply",
                        "command_run": ["provider-bin", "cleanup", "--apply"],
                        "phase": "running",
                        "last_progress": "removed 10/20",
                        "run_refs": [
                            {
                                "run_id": "cleanup-run-1",
                                "status_command": "provider status cleanup-run-1"
                            }
                        ],
                        "follow_up_command": "provider status cleanup-run-1"
                    }
                ]
            }
        });

        let summary = render_worktree_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Worktree provider cleanup summary\n"));
        assert!(summary.contains("Mode: apply\n"));
        assert!(summary.contains("Provider fixture: ok\n"));
        assert!(summary.contains("  Command: provider-bin cleanup --apply\n"));
        assert!(summary.contains("  Phase: running\n"));
        assert!(summary.contains("  Last observed progress: removed 10/20\n"));
        assert!(summary.contains("  Run: cleanup-run-1\n"));
        assert!(summary.contains("  Status command: provider status cleanup-run-1\n"));
        assert!(summary.contains("  Safe follow-up command: provider status cleanup-run-1\n"));
    }
}
