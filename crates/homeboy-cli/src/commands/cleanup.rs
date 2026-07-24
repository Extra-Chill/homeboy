use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

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

    /// Continue a bounded shared-store cleanup inventory from this cursor.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

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
    SharedCargoTargets,
    ControllerRuntimes,
}

#[derive(Subcommand)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),

    /// Aggregate cleanup across configured external worktree providers
    Worktrees(CleanupWorktreesArgs),

    /// Explain retained Homeboy storage without deleting or reconciling resources.
    RetainedStorage(CleanupRetainedStorageArgs),
}

#[derive(Args)]
pub struct CleanupRetainedStorageArgs {
    /// Maximum largest-byte examples to return. The report always aggregates all inspected sources.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Continue largest-byte examples after this deterministic reference token.
    #[arg(long)]
    pub cursor: Option<String>,
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
        Some(CleanupCommand::RetainedStorage(args)) => retained_storage_report(args)
            .and_then(|output| {
                serde_json::to_value(output).map_err(|err| {
                    homeboy::core::Error::internal_json(
                        err.to_string(),
                        Some("serialize retained storage report".to_string()),
                    )
                })
            })
            .map(|output| (output, 0)),
        None => cleanup_inventory(args).map(|output| (output, 0)),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RetainedStorageRecord {
    category: String,
    reason: String,
    owner: String,
    run_id: Option<String>,
    liveness: String,
    age: String,
    age_seconds: Option<u64>,
    size_bytes: u64,
    reference: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct RetainedStorageAggregate {
    key: String,
    count: usize,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct RetainedStorageReport {
    command: &'static str,
    mode: &'static str,
    inspected_count: usize,
    retained_count: usize,
    retained_bytes: u64,
    by_category: Vec<RetainedStorageAggregate>,
    by_reason: Vec<RetainedStorageAggregate>,
    by_owner: Vec<RetainedStorageAggregate>,
    by_liveness: Vec<RetainedStorageAggregate>,
    by_age: Vec<RetainedStorageAggregate>,
    largest_examples: Vec<RetainedStorageRecord>,
    continuation: Option<String>,
    safe_next_commands: Vec<String>,
    sqlite: RetainedStorageSqlite,
}

#[derive(Debug, Serialize)]
struct RetainedStorageSqlite {
    path: String,
    exists: bool,
    size_bytes: u64,
    status_command: &'static str,
    compaction: &'static str,
}

fn retained_storage_report(
    args: CleanupRetainedStorageArgs,
) -> homeboy::core::Result<RetainedStorageReport> {
    if args.limit == 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "limit",
            "--limit must be positive",
            None,
            None,
        ));
    }

    let mut records = Vec::new();
    let runtime = controller_runtime::retention_report()?;
    for snapshot in runtime.snapshots {
        if snapshot.eligible {
            continue;
        }
        records.push(RetainedStorageRecord {
            category: "controller_runtimes".to_string(),
            reason: snapshot.retention_reasons.join(", "),
            owner: snapshot.identity,
            run_id: None,
            liveness: "lifecycle_pinned".to_string(),
            age: age_bucket(snapshot.age_seconds),
            age_seconds: Some(snapshot.age_seconds),
            size_bytes: snapshot.size_bytes,
            reference: snapshot.path.display().to_string(),
        });
    }

    let retention = defaults::load_config().retention;
    let cargo = cleanup::shared_cargo_target_inventory(
        None,
        std::time::SystemTime::now(),
        Duration::from_secs(retention.shared_store_days.saturating_mul(86_400)),
        Duration::from_secs(retention.shared_store_lease_seconds),
    )?;
    for store in cargo {
        let reason = if store
            .reasons
            .iter()
            .any(|reason| reason.starts_with("skipped:"))
        {
            store.reasons.join(", ")
        } else if store.reasons.iter().any(|reason| reason == "active_lease") {
            "active lease".to_string()
        } else if !store.reasons.iter().any(|reason| reason == "age_expired") {
            "within age and size budget".to_string()
        } else {
            continue;
        };
        let liveness = if reason == "active lease" {
            "active"
        } else {
            "unknown"
        };
        records.push(RetainedStorageRecord {
            category: "shared_cargo_targets".to_string(),
            reason,
            owner: store
                .owner
                .unwrap_or_else(|| "unknown/unmanaged".to_string()),
            run_id: None,
            liveness: liveness.to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            size_bytes: store.size_bytes,
            reference: store.path,
        });
    }

    for resource in homeboy::agents::controller_scratch::retained_storage_inventory()? {
        records.push(RetainedStorageRecord {
            category: "controller_scratch".to_string(),
            reason: resource.reason,
            owner: format!("pid {}", resource.owner_pid),
            run_id: Some(resource.run_id),
            liveness: resource.liveness,
            age: resource
                .age_seconds
                .map(age_bucket)
                .unwrap_or_else(|| "unknown".to_string()),
            age_seconds: resource.age_seconds,
            size_bytes: resource.size_bytes,
            reference: format!("{} (task {})", resource.path, resource.task_id),
        });
    }

    let runtime_tmp =
        engine::temp::cleanup_runtime_tmp_bounded(engine::temp::RuntimeTempCleanupOptions {
            apply: false,
            older_than_days: retention.runtime_tmp_days,
            prefix: None,
            limit: usize::MAX,
            run_max_bytes: retention.runtime_run_max_bytes,
            run_max_count: retention.runtime_run_max_count,
            cursor: None,
        })?;
    for row in runtime_tmp
        .rows
        .into_iter()
        .filter(|row| row.owner_id.is_some())
    {
        let liveness = if row
            .protection_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("is running"))
        {
            "active"
        } else if row.owner_state.as_deref() == Some("active") {
            "stale"
        } else {
            "terminal"
        };
        records.push(RetainedStorageRecord {
            category: "runtime_tmp".to_string(),
            reason: row.reason,
            owner: row.owner_id.unwrap_or_else(|| "unknown".to_string()),
            run_id: None,
            liveness: liveness.to_string(),
            age: row
                .age_seconds
                .map(age_bucket)
                .unwrap_or_else(|| "unknown".to_string()),
            age_seconds: row.age_seconds,
            size_bytes: row.size_bytes,
            reference: row.path,
        });
    }

    let database_path = homeboy::core::observation::store::database_path()?;
    let metadata = std::fs::metadata(&database_path).ok();
    let sqlite = RetainedStorageSqlite {
        path: database_path.display().to_string(),
        exists: metadata.is_some(),
        size_bytes: metadata.as_ref().map_or(0, std::fs::Metadata::len),
        status_command: "homeboy db status",
        compaction: "SQLite compaction is explicitly delegated; inspect status before selecting an operator-managed VACUUM workflow.",
    };
    if sqlite.exists {
        records.push(RetainedStorageRecord {
            category: "sqlite_observation_store".to_string(),
            reason: "durable lifecycle database; compaction delegated".to_string(),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: "managed".to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            size_bytes: sqlite.size_bytes,
            reference: sqlite.path.clone(),
        });
    }

    Ok(build_retained_storage_report(
        records,
        args.limit,
        args.cursor.as_deref(),
        sqlite,
    ))
}

fn build_retained_storage_report(
    mut records: Vec<RetainedStorageRecord>,
    limit: usize,
    cursor: Option<&str>,
    sqlite: RetainedStorageSqlite,
) -> RetainedStorageReport {
    records.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.reference.cmp(&right.reference))
    });
    let inspected_count = records.len();
    let retained_bytes = records.iter().map(|record| record.size_bytes).sum();
    let start = cursor
        .and_then(|cursor| records.iter().position(|record| record.reference == cursor))
        .map_or(0, |index| index + 1);
    let examples: Vec<_> = records.iter().skip(start).take(limit).cloned().collect();
    let continuation = (start + examples.len() < records.len()).then(|| {
        let cursor = examples.last().expect("continuation requires an example");
        format!(
            "homeboy cleanup retained-storage --limit {} --cursor {}",
            limit,
            quote_arg(&cursor.reference)
        )
    });
    RetainedStorageReport {
        command: "cleanup.retained_storage",
        mode: "report",
        inspected_count,
        retained_count: inspected_count,
        retained_bytes,
        by_category: aggregate_retained(&records, |record| record.category.clone()),
        by_reason: aggregate_retained(&records, |record| record.reason.clone()),
        by_owner: aggregate_retained(&records, |record| match &record.run_id {
            Some(run_id) => format!("{} (run {run_id})", record.owner),
            None => record.owner.clone(),
        }),
        by_liveness: aggregate_retained(&records, |record| record.liveness.clone()),
        by_age: aggregate_retained(&records, |record| record.age.clone()),
        largest_examples: examples,
        continuation,
        safe_next_commands: vec![
            "homeboy cleanup --include runtime-tmp".to_string(),
            "homeboy cleanup --include controller-scratch".to_string(),
            "homeboy cleanup --include controller-runtimes".to_string(),
            "homeboy cleanup --include shared-cargo-targets".to_string(),
            "homeboy db status".to_string(),
        ],
        sqlite,
    }
}

fn aggregate_retained(
    records: &[RetainedStorageRecord],
    key: impl Fn(&RetainedStorageRecord) -> String,
) -> Vec<RetainedStorageAggregate> {
    let mut totals: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for record in records {
        let entry = totals.entry(key(record)).or_default();
        entry.0 += 1;
        entry.1 += record.size_bytes;
    }
    totals
        .into_iter()
        .map(|(key, (count, size_bytes))| RetainedStorageAggregate {
            key,
            count,
            size_bytes,
        })
        .collect()
}

fn age_bucket(age_seconds: u64) -> String {
    match age_seconds {
        0..=3_599 => "under_1h".to_string(),
        3_600..=86_399 => "under_1d".to_string(),
        86_400..=604_799 => "under_7d".to_string(),
        _ => "7d_or_more".to_string(),
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
    pub runtime_run_max_bytes: u64,
    pub runtime_run_max_count: usize,
    pub shared_store_days: u64,
    pub shared_store_max_bytes: u64,
    pub shared_store_lease_seconds: u64,
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

const SHARED_CARGO_TARGETS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "shared_cargo_targets",
        include_arg: "shared-cargo-targets",
        dry_run_command: "homeboy cleanup --include shared-cargo-targets",
        apply_command: "homeboy cleanup --include shared-cargo-targets --apply",
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

    if selected.includes(CleanupCategoryArg::RepoArtifacts) {
        categories.push(repo_artifacts_category(apply)?);
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
        let output =
            engine::temp::cleanup_runtime_tmp_bounded(engine::temp::RuntimeTempCleanupOptions {
                apply,
                older_than_days: configured.runtime_tmp_days,
                prefix: None,
                limit: usize::try_from(limit).unwrap_or(usize::MAX),
                run_max_bytes: configured.runtime_run_max_bytes,
                run_max_count: configured.runtime_run_max_count,
                cursor: args.cursor.as_deref(),
            })?;
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
                // Thread the operator's explicit `--older-than-days` override
                // into the retention eligibility decision so released, clean,
                // terminal scratch can converge under disk pressure. When the
                // operator does not pass the flag (`None`), preserve the default
                // per-resource retention window (P7D) rather than substituting
                // the configured terminal-run default used by other categories.
                retention_override_seconds: args
                    .older_than_days
                    .map(|days| days.saturating_mul(86_400)),
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
        categories.push(category_from_output(
            CONTROLLER_RUNTIMES_METADATA,
            apply,
            output
                .snapshots
                .iter()
                .filter(|snapshot| snapshot.eligible)
                .count(),
            output.removed_identities.len(),
            output.retained.len(),
            estimated_bytes,
            output.reclaimed_bytes,
            output,
        )?);
    }

    if selected.includes(CleanupCategoryArg::SharedCargoTargets) {
        let output = cleanup::cleanup_shared_cargo_targets(cleanup::CargoTargetCleanupOptions {
            root: None,
            apply,
            older_than: Duration::from_secs(configured.shared_store_days.saturating_mul(86_400)),
            max_bytes: configured.shared_store_max_bytes,
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
            cursor: args.cursor.clone(),
            now: std::time::SystemTime::now(),
            lease_ttl: Duration::from_secs(configured.shared_store_lease_seconds),
        })?;
        categories.push(category_from_output(
            SHARED_CARGO_TARGETS_METADATA,
            apply,
            output.candidate_count,
            output.applied_count,
            output.skipped_count,
            output.candidates.iter().map(|store| store.size_bytes).sum(),
            output.reclaimed_bytes,
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
            runtime_run_max_bytes: configured.runtime_run_max_bytes,
            runtime_run_max_count: configured.runtime_run_max_count,
            shared_store_days: configured.shared_store_days,
            shared_store_max_bytes: configured.shared_store_max_bytes,
            shared_store_lease_seconds: configured.shared_store_lease_seconds,
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
}

#[derive(Debug, Serialize)]
struct RepoArtifactRootDiagnostic {
    scope: &'static str,
    path: Option<String>,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<cleanup::ArtifactCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn repo_artifacts_category(apply: bool) -> homeboy::core::Result<CleanupInventoryCategory> {
    let configured_roots: Vec<PathBuf> = homeboy::core::component::registered()
        .unwrap_or_default()
        .into_iter()
        .map(|component| PathBuf::from(component.local_path))
        .collect();
    let include_source_checkout = configured_roots.is_empty();
    let collected_roots = repo_artifact_roots(configured_roots, include_source_checkout, apply);
    let mut output = cleanup_repo_artifact_roots(collected_roots.roots);
    output.diagnostics.extend(collected_roots.diagnostics);
    if !include_source_checkout
        && output
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.success)
    {
        let source_output =
            cleanup_repo_artifact_roots(repo_artifact_roots(Vec::new(), true, apply).roots);
        output.candidate_count += source_output.candidate_count;
        output.applied_count += source_output.applied_count;
        output.skipped_count += source_output.skipped_count;
        output.estimated_bytes += source_output.estimated_bytes;
        output.reclaimed_bytes += source_output.reclaimed_bytes;
        output.diagnostics.extend(source_output.diagnostics);
    }
    let failure_count = output
        .diagnostics
        .iter()
        .filter(|diagnostic| !diagnostic.success)
        .count();
    Ok(CleanupInventoryCategory {
        category: REPO_ARTIFACTS_METADATA.category,
        canonical_cleanup_command: REPO_ARTIFACTS_METADATA.canonical_cleanup_command(apply),
        specialist_command: REPO_ARTIFACTS_METADATA
            .specialist_command(apply)
            .to_string(),
        included: true,
        skipped: output.candidate_count == 0
            && output
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.success),
        skip_reason: (failure_count > 0)
            .then(|| format!("{failure_count} owned cleanup root(s) could not be inspected")),
        candidate_count: output.candidate_count,
        applied_count: output.applied_count,
        skipped_count: output.skipped_count + failure_count,
        estimated_bytes: output.estimated_bytes,
        reclaimed_bytes: output.reclaimed_bytes,
        output: serde_json::to_value(output.diagnostics).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("repo_artifacts".to_string()),
            )
        })?,
    })
}

struct RepoArtifactRootsCleanup {
    diagnostics: Vec<RepoArtifactRootDiagnostic>,
    candidate_count: usize,
    applied_count: usize,
    skipped_count: usize,
    estimated_bytes: u64,
    reclaimed_bytes: u64,
}

struct RepoArtifactRootCollection {
    roots: Vec<(&'static str, ArtifactCleanupOptions)>,
    diagnostics: Vec<RepoArtifactRootDiagnostic>,
}

fn cleanup_repo_artifact_roots(
    roots: Vec<(&'static str, ArtifactCleanupOptions)>,
) -> RepoArtifactRootsCleanup {
    let mut output = RepoArtifactRootsCleanup {
        diagnostics: Vec::new(),
        candidate_count: 0,
        applied_count: 0,
        skipped_count: 0,
        estimated_bytes: 0,
        reclaimed_bytes: 0,
    };
    for (scope, options) in roots {
        match cleanup::cleanup_artifacts(options) {
            Ok(root_output) => {
                output.candidate_count += root_output.candidate_count;
                output.applied_count += root_output.applied_count;
                output.skipped_count += root_output.skipped_count;
                output.estimated_bytes += root_output.estimated_bytes;
                output.reclaimed_bytes += root_output.reclaimed_bytes;
                output.diagnostics.push(RepoArtifactRootDiagnostic {
                    scope,
                    path: Some(root_output.root.clone()),
                    success: true,
                    output: Some(root_output),
                    error: None,
                });
            }
            Err(error) => output.diagnostics.push(RepoArtifactRootDiagnostic {
                scope,
                path: None,
                success: false,
                output: None,
                error: Some(error.message),
            }),
        }
    }
    output
}

fn repo_artifact_roots(
    configured_roots: Vec<PathBuf>,
    include_source_checkout: bool,
    apply: bool,
) -> RepoArtifactRootCollection {
    let mut collection = RepoArtifactRootCollection {
        roots: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut seen = HashSet::new();
    for path in configured_roots {
        if !path.is_absolute() {
            collection.diagnostics.push(RepoArtifactRootDiagnostic {
                scope: "configured_component",
                path: Some(path.to_string_lossy().to_string()),
                success: false,
                output: None,
                error: Some("configured component local_path must be absolute".to_string()),
            });
            continue;
        }
        let root = homeboy::core::git::repo_root(&path)
            .and_then(|root| std::fs::canonicalize(root).ok())
            .unwrap_or(path);
        if seen.insert(root.clone()) {
            collection.roots.push((
                "configured_component",
                ArtifactCleanupOptions {
                    path: Some(root),
                    apply,
                    self_artifacts: false,
                    temp_roots: Vec::new(),
                    sort: ArtifactCleanupSort::Discovery,
                    limit: None,
                    merged_only: false,
                },
            ));
        }
    }
    if include_source_checkout {
        collection.roots.push((
            "homeboy_source_checkout",
            ArtifactCleanupOptions {
                path: None,
                apply,
                self_artifacts: true,
                temp_roots: Vec::new(),
                sort: ArtifactCleanupSort::Discovery,
                limit: None,
                merged_only: false,
            },
        ));
    }
    collection
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
            if apply || category.category == TASK_WORKTREES_METADATA.category {
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
        payload
            .get("next_command")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "homeboy cleanup artifacts --path {} --apply",
                    quote_arg(root)
                )
            })
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
    use std::process::Command;
    use tempfile::TempDir;

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
    fn retained_storage_cli_accepts_a_bounded_example_limit() {
        let cli = Cli::parse_from([
            "homeboy",
            "cleanup",
            "retained-storage",
            "--limit",
            "3",
            "--cursor",
            "prior-reference",
        ]);

        let Commands::Cleanup(args) = cli.command else {
            panic!("expected cleanup command");
        };
        let Some(CleanupCommand::RetainedStorage(args)) = args.command else {
            panic!("expected retained storage command");
        };
        assert_eq!(args.limit, 3);
        assert_eq!(args.cursor.as_deref(), Some("prior-reference"));
    }

    #[test]
    fn retained_storage_aggregation_is_bounded_and_groups_lifecycle_dimensions() {
        let records = vec![
            RetainedStorageRecord {
                category: "controller_runtimes".to_string(),
                reason: "referenced by recoverable run".to_string(),
                owner: "runtime-a".to_string(),
                run_id: Some("run-1".to_string()),
                liveness: "lifecycle_pinned".to_string(),
                age: "under_1d".to_string(),
                age_seconds: Some(7_200),
                size_bytes: 30,
                reference: "runtime-a".to_string(),
            },
            RetainedStorageRecord {
                category: "shared_cargo_targets".to_string(),
                reason: "active lease".to_string(),
                owner: "cook-1".to_string(),
                run_id: None,
                liveness: "active".to_string(),
                age: "unknown".to_string(),
                age_seconds: None,
                size_bytes: 50,
                reference: "target-a".to_string(),
            },
            RetainedStorageRecord {
                category: "shared_cargo_targets".to_string(),
                reason: "within age and size budget".to_string(),
                owner: "cook-2".to_string(),
                run_id: None,
                liveness: "unknown".to_string(),
                age: "under_7d".to_string(),
                age_seconds: Some(172_800),
                size_bytes: 20,
                reference: "target-b".to_string(),
            },
        ];
        let report = build_retained_storage_report(
            records,
            2,
            None,
            RetainedStorageSqlite {
                path: "homeboy.sqlite".to_string(),
                exists: true,
                size_bytes: 10,
                status_command: "homeboy db status",
                compaction: "delegated",
            },
        );

        assert_eq!(report.retained_count, 3);
        assert_eq!(report.retained_bytes, 100);
        assert_eq!(report.largest_examples.len(), 2);
        assert_eq!(report.largest_examples[0].reference, "target-a");
        assert_eq!(
            report.continuation.as_deref(),
            Some("homeboy cleanup retained-storage --limit 2 --cursor runtime-a")
        );
        assert!(report
            .by_category
            .iter()
            .any(|row| row.key == "shared_cargo_targets"
                && row.count == 2
                && row.size_bytes == 70));
        assert!(report
            .by_owner
            .iter()
            .any(|row| row.key == "runtime-a (run run-1)"));
        assert!(report
            .by_liveness
            .iter()
            .any(|row| row.key == "active" && row.size_bytes == 50));
        assert!(report
            .by_age
            .iter()
            .any(|row| row.key == "under_1d" && row.size_bytes == 30));
        let continuation = build_retained_storage_report(
            report.largest_examples.clone(),
            1,
            Some("target-a"),
            RetainedStorageSqlite {
                path: "homeboy.sqlite".to_string(),
                exists: true,
                size_bytes: 10,
                status_command: "homeboy db status",
                compaction: "delegated",
            },
        );
        assert_eq!(continuation.largest_examples[0].reference, "runtime-a");
        assert_eq!(age_bucket(3_600), "under_1d");
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
    fn aggregate_repo_artifact_roots_do_not_depend_on_the_caller_directory() {
        let configured = vec![
            PathBuf::from("/configured/one"),
            PathBuf::from("/configured/two"),
        ];
        let roots = repo_artifact_roots(configured.clone(), true, false);

        assert_eq!(roots.roots.len(), 3);
        assert_eq!(roots.roots[0].0, "configured_component");
        assert_eq!(roots.roots[0].1.path.as_ref(), Some(&configured[0]));
        assert_eq!(roots.roots[1].0, "configured_component");
        assert_eq!(roots.roots[1].1.path.as_ref(), Some(&configured[1]));
        assert_eq!(roots.roots[2].0, "homeboy_source_checkout");
        assert!(roots.roots[2].1.self_artifacts);
        assert!(roots
            .roots
            .iter()
            .all(|(_, options)| options.path.is_some() || options.self_artifacts));
    }

    #[test]
    fn aggregate_repo_artifact_roots_deduplicate_configured_paths_and_preserve_apply() {
        let root = PathBuf::from("/configured/root");
        let roots = repo_artifact_roots(vec![root.clone(), root], false, true);

        assert_eq!(roots.roots.len(), 1);
        assert_eq!(roots.roots[0].0, "configured_component");
        assert_eq!(
            roots.roots[0].1.path,
            Some(PathBuf::from("/configured/root"))
        );
        assert!(roots.roots[0].1.apply);
    }

    #[test]
    fn aggregate_repo_artifact_roots_reject_relative_persisted_paths() {
        let roots = repo_artifact_roots(vec![PathBuf::from(".")], false, false);

        assert!(roots.roots.is_empty());
        assert_eq!(roots.diagnostics.len(), 1);
        assert_eq!(roots.diagnostics[0].path.as_deref(), Some("."));
        assert_eq!(
            roots.diagnostics[0].error.as_deref(),
            Some("configured component local_path must be absolute")
        );
    }

    #[test]
    fn aggregate_repo_artifact_roots_deduplicate_paths_with_one_git_root() {
        let repository = TempDir::new().expect("repository");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repository.path())
            .output()
            .expect("initialize repository");
        let subdirectory = repository.path().join("packages/component");
        std::fs::create_dir_all(&subdirectory).expect("component directory");

        let roots = repo_artifact_roots(
            vec![subdirectory, repository.path().to_path_buf()],
            false,
            false,
        );

        assert_eq!(roots.roots.len(), 1);
        assert_eq!(
            roots.roots[0].1.path.as_deref(),
            Some(
                repository
                    .path()
                    .canonicalize()
                    .expect("canonical repository")
                    .as_path()
            )
        );
    }

    #[test]
    fn invalid_owned_root_does_not_abort_other_repo_artifact_roots() {
        let repository = TempDir::new().expect("repository");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repository.path())
            .output()
            .expect("initialize repository");
        std::fs::create_dir_all(repository.path().join("target/debug")).expect("target directory");
        std::fs::write(repository.path().join("target/debug/app"), "artifact")
            .expect("target artifact");

        let roots = repo_artifact_roots(
            vec![
                PathBuf::from("/does/not/exist"),
                repository.path().to_path_buf(),
            ],
            false,
            false,
        );
        let output = cleanup_repo_artifact_roots(roots.roots);

        assert_eq!(output.diagnostics.len(), 2);
        assert!(!output.diagnostics[0].success);
        assert!(output.diagnostics[1].success);
        assert_eq!(output.candidate_count, 1);
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
                SHARED_CARGO_TARGETS_METADATA,
                "shared_cargo_targets",
                "shared-cargo-targets",
                "homeboy cleanup --include shared-cargo-targets",
                "homeboy cleanup --include shared-cargo-targets --apply",
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
    fn task_worktree_cleanup_next_actions_preserve_mode_specific_commands() {
        let cases = [
            (
                false,
                "homeboy worktree cleanup --dry-run --cleanup-branches",
            ),
            (true, "homeboy worktree cleanup --cleanup-branches"),
        ];

        for (apply, command) in cases {
            let category = CleanupInventoryCategory {
                category: TASK_WORKTREES_METADATA.category,
                canonical_cleanup_command: TASK_WORKTREES_METADATA.canonical_cleanup_command(apply),
                specialist_command: TASK_WORKTREES_METADATA
                    .specialist_command(apply)
                    .to_string(),
                included: true,
                skipped: false,
                skip_reason: None,
                candidate_count: 1,
                applied_count: 0,
                skipped_count: 0,
                estimated_bytes: 0,
                reclaimed_bytes: 0,
                output: Value::Null,
            };

            let actionable = cleanup_actionable(&[category], apply);
            assert_eq!(actionable.next_actions[0].command, command);
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
            "next_command": "homeboy cleanup artifacts --path '/tmp/homeboy repo' --temp-root /tmp/review --sort size --limit 7 --merged-only --apply",
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
            "Next safe command: homeboy cleanup artifacts --path '/tmp/homeboy repo' --temp-root /tmp/review --sort size --limit 7 --merged-only --apply\n"
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
