use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs4::fs_std::FileExt;
use homeboy::core::cleanup::{
    self, ArtifactCleanupOptions, ArtifactCleanupSort, CleanupPolicy, CleanupPolicyOverrides,
    ResourceCleanupOptions,
};
use homeboy::core::controller_runtime::{self, ControllerRuntimeRetentionOverrides};
use homeboy::core::defaults;
use homeboy::core::engine;
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::observation::runs_service::{
    self, OrphanedArtifactBytesCleanupOptions, PersistedArtifactCleanupOptions,
    RunnerDownloadCleanupOptions,
};
use homeboy::core::output::OutputBudget;
use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::worktree::{self, WorktreeCleanupOptions, WorktreeCleanupOutput};
use homeboy::core::worktree_providers::WorktreeProviderCleanupOptions;
use homeboy::runner::runners::{
    self as runner, RunnerBinaryCachePruneOptions, RunnerBinaryCachePruneOutput,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput,
};
pub use homeboy_command_contract::cleanup::{
    runtime_tmp_commands, CleanupArgs, CleanupArtifactsArgs, CleanupArtifactsSortArg,
    CleanupCategoryArg, CleanupCommand, CleanupInventoryCategoryMetadata,
    CleanupRetainedStorageArgs, CleanupWorktreesArgs, RUNNER_DOWNLOADS_METADATA,
};
use serde::Serialize;
use serde_json::Value;

use super::runs::{runs_resources, RunsOutput, RunsResourcesArgs, RunsResourcesOutput};
use super::utils::response::{CommandActionableMetadata, CommandNextAction, CommandNextActionKind};
use super::CmdResult;

const AUTOMATIC_RETENTION_LOCK_FILE: &str = "automatic-retention-controller.lock";
const AUTOMATIC_RETENTION_STATE_FILE: &str = "automatic-retention-controller.json";

// Advisory file locks are process-scoped on POSIX. Keep the process-local gate
// so a manual invocation cannot overlap a scheduled pass in this process.
static AUTOMATIC_RETENTION_ADMISSION: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Serialize)]
struct AutomaticRetentionControllerOutput {
    command: &'static str,
    status: &'static str,
    state_path: String,
    resume_command: &'static str,
    reconciliation: homeboy::agents::agent_task_service::AgentTaskReconcileReport,
    repo_artifacts: Value,
    cleanup: Value,
}

const AUTOMATIC_RETENTION_CATEGORIES: [CleanupCategoryArg; 6] = [
    CleanupCategoryArg::TerminalRuns,
    CleanupCategoryArg::PersistedRunArtifacts,
    CleanupCategoryArg::OrphanedArtifactBytes,
    CleanupCategoryArg::RuntimeTmp,
    CleanupCategoryArg::ControllerScratch,
    CleanupCategoryArg::ControllerRuntimes,
];

pub fn run(args: CleanupArgs) -> CmdResult<Value> {
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
                    min_age_days: args.min_age_days,
                    include_active_worktrees: args.include_active_worktrees,
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
        Some(CleanupCommand::AutomaticRetention) => automatic_retention(),
        None => cleanup_inventory(args).map(|result| (result.output, result.exit_code)),
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

/// Liveness assigned to storage a cleanup category would reclaim on its next
/// apply. Every other liveness value describes storage cleanup is holding on
/// to, which is what `retained_count`/`retained_bytes` total.
const LIVENESS_RECLAIMABLE: &str = "reclaimable";

#[derive(Debug, Serialize)]
struct RetainedStorageReport {
    command: &'static str,
    mode: &'static str,
    inspected_count: usize,
    retained_count: usize,
    retained_bytes: u64,
    /// Inspected storage a cleanup category would reclaim on its next apply.
    /// Kept separate from the retained totals so "cleanup cannot free this" and
    /// "cleanup has not freed this yet" are never added together.
    reclaimable_count: usize,
    reclaimable_bytes: u64,
    by_category: Vec<RetainedStorageAggregate>,
    by_reason: Vec<RetainedStorageAggregate>,
    by_owner: Vec<RetainedStorageAggregate>,
    by_liveness: Vec<RetainedStorageAggregate>,
    by_age: Vec<RetainedStorageAggregate>,
    largest_examples: Vec<RetainedStorageRecord>,
    continuation: Option<String>,
    /// `false` when a source inventory reached its bounded page before every
    /// record could be accounted for. Aggregates then describe only this page.
    totals_complete: bool,
    source_continuations: Vec<String>,
    safe_next_commands: Vec<String>,
    sqlite: RetainedStorageSqlite,
    /// Added in #9824. Existing lifecycle aggregates above retain their
    /// historical meaning; this is the direct filesystem reconciliation view.
    filesystem: RetainedStorageFilesystem,
}

#[derive(Debug, Serialize)]
struct RetainedStorageSqlite {
    path: String,
    exists: bool,
    size_bytes: u64,
    status_command: &'static str,
    compaction: &'static str,
}

#[derive(Debug, Serialize)]
struct RetainedStorageFilesystem {
    root: RetainedStorageFilesystemUsage,
    top_level: Vec<RetainedStorageFilesystemEntry>,
    reconciliation: RetainedStorageReconciliation,
    accounting_notes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct RetainedStorageFilesystemUsage {
    path: String,
    exists: bool,
    apparent_bytes: u64,
    physical_bytes: u64,
}

#[derive(Debug, Serialize)]
struct RetainedStorageFilesystemEntry {
    path: String,
    category: &'static str,
    classification: &'static str,
    apparent_bytes: u64,
    physical_bytes: u64,
    cleanup_or_status_command: &'static str,
    largest_examples: Vec<RetainedStorageFilesystemUsage>,
}

#[derive(Debug, Serialize)]
struct RetainedStorageReconciliation {
    top_level_apparent_bytes: u64,
    top_level_physical_bytes: u64,
    apparent_difference_bytes: u64,
    physical_difference_bytes: u64,
    apparent_difference_direction: &'static str,
    physical_difference_direction: &'static str,
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

    // The report reads the same resolved policy the delete paths use, so an
    // operator cannot be shown a window cleanup would not actually apply.
    let policy = cleanup::resolve_cleanup_policy(CleanupPolicyOverrides::default())?;
    let cargo = cleanup::shared_cargo_target_inventory(
        None,
        std::time::SystemTime::now(),
        policy.shared_store_min_age(),
        policy.shared_store_lease_ttl(),
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
            older_than_days: policy.runtime_tmp_days,
            managed_older_than_days: None,
            prefix: None,
            // Retained-storage is an operator report, not an unbounded disk
            // walk. It shares the cleanup record budget and names a cursor for
            // the next runtime-temp page below.
            limit: policy.scan_limit(),
            run_max_bytes: policy.runtime_run_max_bytes,
            run_max_count: policy.runtime_run_max_count,
            cursor: None,
        })?;
    let runtime_tmp_continuation = runtime_tmp.next_cursor.as_ref().map(|cursor| {
        format!(
            "homeboy cleanup --include runtime-tmp --cursor {}",
            quote_arg(cursor)
        )
    });
    records.extend(
        runtime_tmp
            .rows
            .into_iter()
            .map(runtime_tmp_retained_record),
    );

    records.extend(artifact_root_records(policy)?);

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
            // Explicitly scoped: this is the index, not the bytes it indexes.
            // Artifact payloads are accounted for by the artifact-root
            // categories above and dwarf the database itself.
            reason: "durable lifecycle database (index only, not indexed artifact bytes); compaction delegated".to_string(),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: "managed".to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            size_bytes: sqlite.size_bytes,
            reference: sqlite.path.clone(),
        });
    }

    let filesystem = retained_storage_filesystem_inventory()?;
    let mut report = build_retained_storage_report(
        records,
        args.limit,
        args.cursor.as_deref(),
        sqlite,
        filesystem,
    );
    if let Some(command) = runtime_tmp_continuation {
        report.totals_complete = false;
        report.source_continuations.push(command);
    }
    Ok(report)
}

fn runtime_tmp_retained_record(row: engine::temp::RuntimeTempCleanupRow) -> RetainedStorageRecord {
    let liveness = if row
        .protection_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("is running"))
    {
        "active"
    } else if row.owner_state.as_deref() == Some("active") {
        "stale"
    } else if row.owner_id.is_none() {
        "external_unknown"
    } else {
        "terminal"
    };
    let owner = match (row.producer.as_deref(), row.owner_id.as_deref()) {
        (Some(producer), Some(owner_id)) => format!("{producer} ({owner_id})"),
        (None, Some(owner_id)) => owner_id.to_string(),
        (_, None) => "external/unattributed".to_string(),
    };
    RetainedStorageRecord {
        category: "runtime_tmp".to_string(),
        reason: format!(
            "{}; cleanup: homeboy cleanup --include runtime-tmp",
            row.reason
        ),
        owner,
        run_id: row.run_id,
        liveness: liveness.to_string(),
        age: row
            .age_seconds
            .map(age_bucket)
            .unwrap_or_else(|| "unknown".to_string()),
        age_seconds: row.age_seconds,
        size_bytes: row.size_bytes,
        reference: row.path,
    }
}

fn retained_storage_filesystem_inventory() -> homeboy::core::Result<RetainedStorageFilesystem> {
    let root = homeboy::core::paths::homeboy_data()?;
    let artifact_root = homeboy::core::artifacts::root()?;
    let cargo_target_root = cleanup::shared_cargo_target_root()?;
    retained_storage_filesystem_inventory_for(root, artifact_root, cargo_target_root)
}

fn retained_storage_filesystem_inventory_for(
    root: PathBuf,
    artifact_root: PathBuf,
    cargo_target_root: PathBuf,
) -> homeboy::core::Result<RetainedStorageFilesystem> {
    let root_usage = filesystem_usage(&root)?;
    let mut top_level = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(&root).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!("read Homeboy storage root {}", root.display())),
            )
        })? {
            let entry = entry.map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(format!("read Homeboy storage root {}", root.display())),
                )
            })?;
            top_level.push(filesystem_entry(entry.path())?);
        }
    }

    // An explicit artifact root can live on a separate volume. It is still a
    // Homeboy store, but cannot be reconciled into the data-root totals.
    if !artifact_root.starts_with(&root) {
        top_level.push(filesystem_entry_with_category(
            artifact_root,
            "artifacts",
            "managed/external",
            "homeboy cleanup --include persisted-run-artifacts",
        )?);
    }
    if !cargo_target_root.starts_with(&root) {
        top_level.push(filesystem_entry_with_category(
            cargo_target_root,
            "shared_cargo_targets",
            "managed/external",
            "homeboy cleanup --include shared-cargo-targets",
        )?);
    }
    top_level.sort_by_key(|entry| std::cmp::Reverse(entry.physical_bytes));

    let apparent_total = top_level
        .iter()
        .filter(|entry| entry.classification != "managed/external")
        .map(|entry| entry.apparent_bytes)
        .sum();
    let physical_total = top_level
        .iter()
        .filter(|entry| entry.classification != "managed/external")
        .map(|entry| entry.physical_bytes)
        .sum();
    Ok(RetainedStorageFilesystem {
        root: RetainedStorageFilesystemUsage {
            path: root.display().to_string(),
            exists: root.exists(),
            apparent_bytes: root_usage.apparent_bytes,
            physical_bytes: root_usage.physical_bytes,
        },
        top_level,
        reconciliation: RetainedStorageReconciliation {
            top_level_apparent_bytes: apparent_total,
            top_level_physical_bytes: physical_total,
            apparent_difference_bytes: root_usage.apparent_bytes.abs_diff(apparent_total),
            physical_difference_bytes: root_usage.physical_bytes.abs_diff(physical_total),
            apparent_difference_direction: difference_direction(root_usage.apparent_bytes, apparent_total),
            physical_difference_direction: difference_direction(root_usage.physical_bytes, physical_total),
        },
        accounting_notes: vec![
            "Apparent bytes sum file lengths; physical bytes sum allocated filesystem blocks.",
            "Sparse files can have greater apparent than physical bytes.",
            "Root totals de-duplicate hard-linked inodes; independently measured top-level stores can count a cross-store hard link more than once.",
            "The root directory's own metadata and filesystem allocation granularity can leave a small reconciliation difference.",
            "Symlinks are measured as links and are not followed.",
        ],
    })
}

fn difference_direction(root: u64, top_level: u64) -> &'static str {
    match root.cmp(&top_level) {
        std::cmp::Ordering::Equal => "equal",
        std::cmp::Ordering::Greater => "root_greater",
        std::cmp::Ordering::Less => "top_level_greater",
    }
}

fn filesystem_entry(path: PathBuf) -> homeboy::core::Result<RetainedStorageFilesystemEntry> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (category, classification, command) = match name {
        "artifacts" => (
            "artifacts",
            "managed",
            "homeboy cleanup --include persisted-run-artifacts",
        ),
        homeboy::core::paths::CARGO_TARGETS_STORE => (
            "shared_cargo_targets",
            "managed/shared",
            "homeboy cleanup --include shared-cargo-targets",
        ),
        homeboy::core::paths::CONTROLLER_RUNTIMES_STORE => (
            "controller_runtimes",
            "managed",
            "homeboy cleanup --include controller-runtimes",
        ),
        homeboy::core::paths::CONTROLLER_SCRATCH_STORE => (
            "controller_scratch",
            "managed",
            "homeboy cleanup --include controller-scratch",
        ),
        "homeboy.sqlite" => ("sqlite_observation_store", "managed", "homeboy db status"),
        _ => (
            "unknown_storage",
            "unknown/unmanaged",
            "inspect path ownership before removal",
        ),
    };
    filesystem_entry_with_category(path, category, classification, command)
}

fn filesystem_entry_with_category(
    path: PathBuf,
    category: &'static str,
    classification: &'static str,
    command: &'static str,
) -> homeboy::core::Result<RetainedStorageFilesystemEntry> {
    let usage = filesystem_usage(&path)?;
    Ok(RetainedStorageFilesystemEntry {
        path: path.display().to_string(),
        category,
        classification,
        apparent_bytes: usage.apparent_bytes,
        physical_bytes: usage.physical_bytes,
        cleanup_or_status_command: command,
        largest_examples: largest_children(&path)?,
    })
}

#[derive(Default)]
struct FilesystemUsage {
    apparent_bytes: u64,
    physical_bytes: u64,
}

fn filesystem_usage(path: &Path) -> homeboy::core::Result<FilesystemUsage> {
    let mut seen = HashSet::new();
    let mut usage = FilesystemUsage::default();
    accumulate_filesystem_usage(path, &mut seen, &mut usage)?;
    Ok(usage)
}

fn accumulate_filesystem_usage(
    path: &Path,
    seen: &mut HashSet<(u64, u64)>,
    usage: &mut FilesystemUsage,
) -> homeboy::core::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!("measure Homeboy storage path {}", path.display())),
            ))
        }
    };
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let identity = (0, 0);
    if seen.insert(identity) {
        usage.apparent_bytes = usage.apparent_bytes.saturating_add(metadata.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            usage.physical_bytes = usage
                .physical_bytes
                .saturating_add(metadata.blocks().saturating_mul(512));
        }
        #[cfg(not(unix))]
        {
            usage.physical_bytes = usage.physical_bytes.saturating_add(metadata.len());
        }
    }
    if metadata.file_type().is_dir() {
        for entry in std::fs::read_dir(path).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!("read Homeboy storage path {}", path.display())),
            )
        })? {
            let entry = entry.map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(format!("read Homeboy storage path {}", path.display())),
                )
            })?;
            accumulate_filesystem_usage(&entry.path(), seen, usage)?;
        }
    }
    Ok(())
}

fn largest_children(path: &Path) -> homeboy::core::Result<Vec<RetainedStorageFilesystemUsage>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(homeboy::core::Error::internal_io(error.to_string(), None)),
    };
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }
    let mut children = Vec::new();
    for entry in std::fs::read_dir(path)
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?
    {
        let entry =
            entry.map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
        let usage = filesystem_usage(&entry.path())?;
        children.push(RetainedStorageFilesystemUsage {
            path: entry.path().display().to_string(),
            exists: true,
            apparent_bytes: usage.apparent_bytes,
            physical_bytes: usage.physical_bytes,
        });
    }
    children.sort_by_key(|entry| std::cmp::Reverse(entry.physical_bytes));
    children.truncate(10);
    Ok(children)
}

/// Account for the artifact root — the product's primary output store.
///
/// `cleanup retained-storage` used to accumulate from five sources and never
/// reach `artifacts::root()`, so the one command whose purpose is "where did my
/// disk go" was blind to the largest store on the box (#10316).
///
/// Every producer here runs its category planner with `apply: false`. These are
/// read-only plans: nothing in this function deletes, and none of the reported
/// sizes are used as a deletion predicate by anyone. Bytes a planner would
/// reclaim are reported as [`LIVENESS_RECLAIMABLE`] so they are never summed
/// into the "cleanup cannot free this" total.
fn artifact_root_records(
    policy: CleanupPolicy,
) -> homeboy::core::Result<Vec<RetainedStorageRecord>> {
    let mut records = Vec::new();

    let persisted = runs_service::cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
        apply: false,
        older_than_days: policy.terminal_run_days,
        run_id: None,
        kind: None,
        artifact_type: None,
        run_kind: None,
        component_id: None,
        limit: policy.limit,
        terminal_only: true,
    })?;
    let artifact_root = persisted.artifact_root.display().to_string();
    for row in persisted.rows {
        // Only `remove` rows carry a measured size; `skip` rows are classified
        // before any measurement happens. Reporting a zero-byte row for every
        // protected artifact would bury the real answer, so retained rows are
        // summarized as one counted record instead of one row each.
        if row.action != "remove" {
            continue;
        }
        let owner = row
            .component_id
            .clone()
            .unwrap_or_else(|| format!("run kind {}", row.run_kind));
        records.push(RetainedStorageRecord {
            category: "persisted_run_artifacts".to_string(),
            reason: row.reason,
            owner,
            run_id: Some(row.run_id),
            liveness: LIVENESS_RECLAIMABLE.to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            size_bytes: row.size_bytes,
            reference: format!("{artifact_root}/{}", row.path),
        });
    }
    let protected = persisted.skipped_count;
    if protected > 0 {
        records.push(RetainedStorageRecord {
            category: "persisted_run_artifacts".to_string(),
            reason: format!(
                "{protected} artifact(s) protected by an active, unknown, remote, or unsafe-path run; bytes not measured"
            ),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: "lifecycle_pinned".to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            // Advisory-signal rule: an unmeasured size is reported as zero and
            // never inferred. It must not move a verdict in either direction.
            size_bytes: 0,
            reference: format!("{artifact_root} (protected persisted artifacts)"),
        });
    }

    // The whole cache used to be reported as reclaimable, because the category
    // deleted all of it unconditionally. It now reports the two halves its
    // predicate actually produces (#10564): what a sweep would reclaim, and
    // what it is holding on to.
    let downloads = runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
        apply: false,
        runner: None,
        run_id: None,
        limit: policy.scan_limit(),
    })?;
    let downloads_root = downloads.root.display().to_string();
    if downloads.planned_count > 0 {
        records.push(RetainedStorageRecord {
            category: "runner_downloads".to_string(),
            reason: format!(
                "{} cached runner download(s) past the fixed {}s age floor with no non-terminal owning run ({} file(s), {} directory(ies))",
                downloads.planned_count,
                downloads.min_age_seconds,
                downloads.file_count,
                downloads.directory_count
            ),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: LIVENESS_RECLAIMABLE.to_string(),
            age: age_bucket(downloads.min_age_seconds),
            age_seconds: Some(downloads.min_age_seconds),
            size_bytes: downloads.planned_size_bytes,
            reference: downloads_root.clone(),
        });
    }
    if downloads.skipped_count > 0 {
        records.push(RetainedStorageRecord {
            category: "runner_downloads".to_string(),
            reason: format!(
                "{} cached runner download(s) retained: younger than the age floor, claimed by a non-terminal run, or not the canonical <runner>/<run> shape; bytes not measured",
                downloads.skipped_count
            ),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: "lifecycle_pinned".to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            // Advisory-signal rule: retained entries are deliberately not
            // measured, so a zero here is "not measured", never "empty".
            size_bytes: 0,
            reference: format!("{downloads_root} (retained runner downloads)"),
        });
    }

    let orphaned =
        runs_service::cleanup_orphaned_artifact_bytes(OrphanedArtifactBytesCleanupOptions {
            apply: false,
            limit: policy.scan_limit(),
        })?;
    if orphaned.planned_count > 0 {
        records.push(RetainedStorageRecord {
            category: "orphaned_artifact_bytes".to_string(),
            reason: format!(
                "{} crash-residue path(s) past the fixed {}s age floor",
                orphaned.planned_count, orphaned.min_age_seconds
            ),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: LIVENESS_RECLAIMABLE.to_string(),
            age: age_bucket(orphaned.min_age_seconds),
            age_seconds: Some(orphaned.min_age_seconds),
            size_bytes: orphaned.planned_size_bytes,
            reference: format!(
                "{} (orphaned artifact bytes)",
                orphaned.artifact_root.display()
            ),
        });
    }

    Ok(records)
}

fn build_retained_storage_report(
    mut records: Vec<RetainedStorageRecord>,
    limit: usize,
    cursor: Option<&str>,
    sqlite: RetainedStorageSqlite,
    filesystem: RetainedStorageFilesystem,
) -> RetainedStorageReport {
    records.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.reference.cmp(&right.reference))
    });
    let inspected_count = records.len();
    let (reclaimable, retained): (Vec<_>, Vec<_>) = records
        .iter()
        .partition(|record| record.liveness == LIVENESS_RECLAIMABLE);
    let retained_count = retained.len();
    let retained_bytes = retained.iter().map(|record| record.size_bytes).sum();
    let reclaimable_count = reclaimable.len();
    let reclaimable_bytes = reclaimable.iter().map(|record| record.size_bytes).sum();
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
        retained_count,
        retained_bytes,
        reclaimable_count,
        reclaimable_bytes,
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
        totals_complete: true,
        source_continuations: Vec::new(),
        // One entry per category this report can account for. Omitting the
        // artifact-root categories used to leave an operator staring at bytes
        // the report itself could not name a reclaim command for.
        safe_next_commands: vec![
            "homeboy cleanup --include runtime-tmp".to_string(),
            "homeboy cleanup --include controller-scratch".to_string(),
            "homeboy cleanup --include controller-runtimes".to_string(),
            "homeboy cleanup --include shared-cargo-targets".to_string(),
            "homeboy cleanup --include persisted-run-artifacts".to_string(),
            "homeboy cleanup --include runner-downloads".to_string(),
            "homeboy cleanup --include orphaned-artifact-bytes".to_string(),
            "homeboy db status".to_string(),
        ],
        sqlite,
        filesystem,
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
    /// `partial_failure` means independent categories completed, but at least
    /// one category failed and can be retried through its specialist command.
    pub status: &'static str,
    pub mode: &'static str,
    pub category_count: usize,
    pub failed_category_count: usize,
    /// Resources selected for removal, summed across categories.
    ///
    /// `candidate_count`, `applied_count` and `skipped_count` all count
    /// *resources*. Category-level execution success is reported separately as
    /// `applied_category_count`, so an atomic directory-level cleanup can no
    /// longer be mistaken for a partial one (#9483).
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    /// Categories that executed and removed at least one resource. Counted in
    /// categories, never in resources.
    pub applied_category_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub retention: CleanupRetentionManifest,
    pub categories: Vec<CleanupInventoryCategory>,
    #[serde(rename = "_homeboy_actionable")]
    pub actionable: CommandActionableMetadata,
}

/// Stable, serialized policy snapshot for a cleanup plan or apply result.
///
/// This is the resolved policy itself rather than a copy of it, so the reported
/// manifest cannot describe a window the deletion did not apply.
pub type CleanupRetentionManifest = CleanupPolicy;

#[derive(Debug, Serialize)]
pub struct CleanupInventoryCategory {
    pub category: &'static str,
    pub canonical_cleanup_command: String,
    pub specialist_command: String,
    pub included: bool,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<CleanupInventoryCategoryFailure>,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub output: Value,
}

#[derive(Debug, Serialize)]
pub struct CleanupInventoryCategoryFailure {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

struct CleanupInventoryResult {
    output: Value,
    exit_code: i32,
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
        dry_run_command: "homeboy worktree cleanup --cleanup-branches",
        apply_command: "homeboy worktree cleanup --cleanup-branches --apply",
    };

const WORKTREE_PROVIDERS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "worktree_providers",
        include_arg: "worktree-providers",
        dry_run_command: "homeboy cleanup worktrees --all-providers",
        apply_command: "homeboy cleanup worktrees --all-providers --apply",
    };

const PERSISTED_RUN_ARTIFACTS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "persisted_run_artifacts",
        include_arg: "persisted-run-artifacts",
        dry_run_command: "homeboy runs artifact cleanup-persisted",
        apply_command: "homeboy runs artifact cleanup-persisted --apply",
    };

/// `homeboy runs retention` was deleted in #10316. It was the one specialist
/// with no narrowing argument left — its `--apply`, `--older-than-days`, and
/// `--limit` are exactly the aggregate's, so it was a second name for one
/// operation rather than a different one. The canonical surface is the only
/// surface.
const TERMINAL_RUNS_METADATA: CleanupInventoryCategoryMetadata = CleanupInventoryCategoryMetadata {
    category: "terminal_runs",
    include_arg: "terminal-runs",
    dry_run_command: "homeboy cleanup --include terminal-runs",
    apply_command: "homeboy cleanup --include terminal-runs --apply",
};

/// Crash residue under the artifact root that no database row can describe.
/// This is the only artifact-root cleanup that is not row-driven, so it is
/// scoped to the two name families a single private constructor owns rather
/// than to "anything without a row" — see
/// `runs_service::orphaned_artifact_bytes` for why a row join is unsafe here.
const ORPHANED_ARTIFACT_BYTES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "orphaned_artifact_bytes",
        include_arg: "orphaned-artifact-bytes",
        dry_run_command: "homeboy cleanup --include orphaned-artifact-bytes",
        apply_command: "homeboy cleanup --include orphaned-artifact-bytes --apply",
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

const RUNNER_BINARY_CACHES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "runner_binary_caches",
        include_arg: "runner-binary-caches",
        dry_run_command: "homeboy runner cache-prune <runner>",
        apply_command: "homeboy runner cache-prune <runner> --apply",
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
        dry_run_command: "homeboy runtime controller-prune",
        apply_command: "homeboy runtime controller-prune --apply",
    };

fn automatic_retention() -> CmdResult<Value> {
    let data = homeboy::core::paths::homeboy_data()?;
    fs::create_dir_all(&data).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("create automatic retention state directory".to_string()),
        )
    })?;
    let state_path = data.join(AUTOMATIC_RETENTION_STATE_FILE);
    let admission = AUTOMATIC_RETENTION_ADMISSION
        .get_or_init(|| Mutex::new(()))
        .try_lock();
    let Ok(_admission) = admission else {
        return Ok((
            serde_json::json!({
                "command": "cleanup.automatic_retention",
                "status": "busy",
                "state_path": state_path,
                "resume_command": "homeboy cleanup automatic-retention",
            }),
            0,
        ));
    };
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(data.join(AUTOMATIC_RETENTION_LOCK_FILE))
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("open automatic retention lock".to_string()),
            )
        })?;
    if lock.try_lock_exclusive().is_err() {
        return Ok((
            serde_json::json!({
                "command": "cleanup.automatic_retention",
                "status": "busy",
                "state_path": state_path,
                "resume_command": "homeboy cleanup automatic-retention",
            }),
            0,
        ));
    }

    fs::write(&state_path, r#"{"status":"running"}"#).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("write automatic retention state".to_string()),
        )
    })?;
    let reconciliation = homeboy::agents::agent_task_service::reconcile_stale_active_runs(false)?;
    let roots = homeboy::core::component::registered()
        .unwrap_or_default()
        .into_iter()
        .map(|component| PathBuf::from(component.local_path))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let repo_artifacts = if roots.is_empty() {
        serde_json::json!({
            "status": "retained",
            "reason": "no controller-accessible registered workspace roots",
        })
    } else {
        match cleanup::run_automatic_artifact_retention(roots) {
            Ok(output) => serde_json::to_value(output).map_err(|error| {
                homeboy::core::Error::internal_json(
                    error.to_string(),
                    Some("serialize automatic artifact retention".to_string()),
                )
            })?,
            Err(error) => serde_json::json!({ "status": "retained", "reason": error.message }),
        }
    };
    let cleanup = cleanup_inventory(CleanupArgs {
        apply: true,
        include: AUTOMATIC_RETENTION_CATEGORIES.to_vec(),
        exclude: Vec::new(),
        older_than_days: None,
        runtime_tmp_managed_older_than_days: None,
        limit: None,
        full: false,
        cursor: None,
        command: None,
    })?;
    let cargo_targets = cleanup::run_automatic_cargo_retention()?;
    let cleanup_exit_code = cleanup.exit_code;
    let status = if cleanup_exit_code == 0 {
        cargo_targets.status
    } else {
        "partial_failure"
    };
    let output = AutomaticRetentionControllerOutput {
        command: "cleanup.automatic_retention",
        status,
        state_path: state_path.display().to_string(),
        resume_command: "homeboy cleanup automatic-retention",
        reconciliation,
        repo_artifacts,
        cleanup: serde_json::json!({
            "categories": cleanup.output,
            "cargo_targets": cargo_targets,
        }),
    };
    let value = serde_json::to_value(&output).map_err(|error| {
        homeboy::core::Error::internal_json(
            error.to_string(),
            Some("serialize automatic retention output".to_string()),
        )
    })?;
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&value).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize automatic retention state".to_string()),
            )
        })?,
    )
    .map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("write automatic retention state".to_string()),
        )
    })?;
    Ok((value, cleanup_exit_code))
}

fn cleanup_inventory(args: CleanupArgs) -> homeboy::core::Result<CleanupInventoryResult> {
    let selected = CleanupCategorySelection::new(args.include.clone(), args.exclude.clone());
    let apply = args.apply;
    let config = defaults::load_config();
    // One resolver for every category and every specialist command. The
    // aggregate no longer derives its own windows.
    let policy = cleanup::cleanup_policy_from_retention(
        &config.retention,
        CleanupPolicyOverrides {
            terminal_run_days: args.older_than_days,
            runtime_tmp_managed_days: args.runtime_tmp_managed_older_than_days,
            limit: args.limit,
            ..CleanupPolicyOverrides::default()
        },
    )?;
    let terminal_run_days = policy.terminal_run_days;
    let limit = policy.limit;
    let mut categories = Vec::new();

    if selected.includes(CleanupCategoryArg::RepoArtifacts) {
        isolate_cleanup_category(
            &mut categories,
            REPO_ARTIFACTS_METADATA,
            apply,
            None,
            None,
            || repo_artifacts_category(apply).map(|category| vec![category]),
        );
    }

    if selected.includes(CleanupCategoryArg::TaskWorktrees) {
        isolate_cleanup_category(
            &mut categories,
            TASK_WORKTREES_METADATA,
            apply,
            None,
            None,
            || {
                let output = worktree::cleanup(WorktreeCleanupOptions {
                    force: false,
                    dry_run: !apply,
                    cleanup_branches: apply,
                    allow_unmerged_branches: false,
                })?;
                task_worktrees_category(output, apply).map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::WorktreeProviders) {
        isolate_cleanup_category(
            &mut categories,
            WORKTREE_PROVIDERS_METADATA,
            apply,
            None,
            None,
            || {
                let output = cleanup::cleanup_resources_from_config(
                    ResourceCleanupOptions {
                        intent: cleanup_intent(apply),
                        artifacts: None,
                        worktree_providers: Some(WorktreeProviderCleanupOptions {
                            provider: Vec::new(),
                            all_providers: true,
                            apply,
                        }),
                    },
                    config.clone(),
                )?;
                category_from_output(
                    WORKTREE_PROVIDERS_METADATA,
                    apply,
                    0,
                    0,
                    output.failure_count,
                    0,
                    0,
                    output,
                )
                .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::TerminalRuns) {
        isolate_cleanup_category(
            &mut categories,
            TERMINAL_RUNS_METADATA,
            apply,
            None,
            None,
            || {
                let output = runs_service::retain_terminal_runs(
                    runs_service::TerminalRunRetentionOptions {
                        apply,
                        older_than_days: terminal_run_days,
                        limit,
                    },
                )?;
                let lifecycle_bytes = output
                    .lifecycle_directories
                    .iter()
                    .map(|directory| directory.size_bytes)
                    .sum();
                category_from_output(
                    TERMINAL_RUNS_METADATA,
                    apply,
                    output.candidate_run_ids.len(),
                    output.removed_run_count,
                    output.skipped_run_ids.len(),
                    lifecycle_bytes,
                    if apply { lifecycle_bytes } else { 0 },
                    output,
                )
                .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::PersistedRunArtifacts) {
        isolate_cleanup_category(
            &mut categories,
            PERSISTED_RUN_ARTIFACTS_METADATA,
            apply,
            None,
            None,
            || {
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
                persisted_artifacts_category(persisted, resources, apply)
                    .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::OrphanedArtifactBytes) {
        isolate_cleanup_category(
            &mut categories,
            ORPHANED_ARTIFACT_BYTES_METADATA,
            apply,
            None,
            None,
            || {
                let mut output = runs_service::cleanup_orphaned_artifact_bytes(
                    OrphanedArtifactBytesCleanupOptions {
                        apply,
                        // Orphaned artifacts have no cursor. Restricting their source
                        // scan would leave later entries unreachable, so execution
                        // always receives the configured retention limit.
                        limit: policy.scan_limit(),
                    },
                )?;
                runs_service::present_orphaned_artifact_bytes_cleanup(
                    &mut output,
                    args.full,
                    cleanup_replay_command(&args, true, true),
                )?;
                category_from_output(
                    ORPHANED_ARTIFACT_BYTES_METADATA,
                    apply,
                    output.planned_count,
                    output.removed_count,
                    output.skipped_count,
                    output.planned_size_bytes,
                    output.removed_size_bytes,
                    output,
                )
                .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::RunnerDownloads) {
        isolate_cleanup_category(
            &mut categories,
            RUNNER_DOWNLOADS_METADATA,
            apply,
            None,
            None,
            || {
                let output =
                    runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
                        apply,
                        runner: None,
                        run_id: None,
                        limit: policy.scan_limit(),
                    })?;
                // `planned_count`, not `inspected_count`: a candidate is a resource
                // selected for removal, not every entry the sweep walked past. Using
                // the inspected total reported `candidate_count: 588, applied_count: 1`
                // for a sweep that removed everything it selected, which reads as a
                // 587-resource failure (#9483). The full inspected total remains
                // visible in this category's nested `output`.
                category_from_output(
                    RUNNER_DOWNLOADS_METADATA,
                    apply,
                    output.planned_count,
                    output.removed_count,
                    output.skipped_count,
                    output.planned_size_bytes,
                    output.removed_size_bytes,
                    output,
                )
                .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::RemoteLabWorkspaces) {
        isolate_cleanup_category(
            &mut categories,
            REMOTE_LAB_WORKSPACES_METADATA,
            apply,
            None,
            None,
            || remote_lab_workspace_categories(policy, apply),
        );
    }

    if selected.includes(CleanupCategoryArg::RunnerBinaryCaches) {
        isolate_cleanup_category(
            &mut categories,
            RUNNER_BINARY_CACHES_METADATA,
            apply,
            None,
            None,
            || runner_binary_cache_categories(policy, apply),
        );
    }

    if selected.includes(CleanupCategoryArg::RuntimeTmp) {
        let commands = args
            .runtime_tmp_managed_older_than_days
            .map(|days| runtime_tmp_commands(apply, days));
        let canonical_cleanup_command = commands.as_ref().map(|(canonical, _)| canonical.clone());
        let specialist_command = commands.as_ref().map(|(_, specialist)| specialist.clone());
        isolate_cleanup_category(
            &mut categories,
            RUNTIME_TMP_METADATA,
            apply,
            canonical_cleanup_command.as_deref(),
            specialist_command.as_deref(),
            || {
                let mut output = engine::temp::cleanup_runtime_tmp_bounded(
                    engine::temp::RuntimeTempCleanupOptions {
                        apply,
                        older_than_days: policy.runtime_tmp_days,
                        managed_older_than_days: Some(policy.runtime_tmp_managed_days),
                        prefix: None,
                        limit: if apply || args.full {
                            policy.scan_limit()
                        } else {
                            policy.scan_limit().min(OutputBudget::COLLECTION.max_items)
                        },
                        run_max_bytes: policy.runtime_run_max_bytes,
                        run_max_count: policy.runtime_run_max_count,
                        cursor: args.cursor.as_deref(),
                    },
                )?;
                engine::temp::present_runtime_temp_cleanup(
                    &mut output,
                    args.full,
                    cleanup_replay_command(&args, false, false),
                    cleanup_replay_command(&args, true, true),
                )?;
                let mut category = category_from_output(
                    RUNTIME_TMP_METADATA,
                    apply,
                    output.planned_count,
                    output.removed_count,
                    output.skipped_count,
                    output.totals.planned_size_bytes,
                    output.totals.removed_size_bytes,
                    output,
                )?;
                if let Some((canonical, specialist)) = commands {
                    (
                        category.canonical_cleanup_command,
                        category.specialist_command,
                    ) = (canonical, specialist);
                }
                Ok(vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::ControllerScratch) {
        isolate_cleanup_category(
            &mut categories,
            CONTROLLER_SCRATCH_METADATA,
            apply,
            None,
            None,
            || {
                let output = homeboy::agents::controller_scratch::cleanup(
                    homeboy::agents::controller_scratch::ControllerScratchCleanupOptions {
                        apply,
                        limit: policy.scan_limit(),
                        full: args.full,
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
                category_from_output(
                    CONTROLLER_SCRATCH_METADATA,
                    apply,
                    output.candidate_count,
                    output.applied_count,
                    output.skipped_count,
                    output.estimated_bytes,
                    output.reclaimed_bytes,
                    output,
                )
                .map(|category| vec![category])
            },
        );
    }
    if selected.includes(CleanupCategoryArg::ControllerRuntimes) {
        isolate_cleanup_category(
            &mut categories,
            CONTROLLER_RUNTIMES_METADATA,
            apply,
            None,
            None,
            || {
                // Policy is resolved by core so this path and `homeboy runtime
                // controller-prune` cannot drift apart again (#10288).
                let output =
                    controller_runtime::cleanup(controller_runtime::resolve_cleanup_options(
                        apply,
                        ControllerRuntimeRetentionOverrides {
                            limit: Some(limit),
                            ignore_retention: false,
                        },
                    ))?;
                let estimated_bytes = output
                    .snapshots
                    .iter()
                    .filter(|snapshot| snapshot.eligible)
                    .map(|snapshot| snapshot.size_bytes)
                    .sum();
                category_from_output(
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
                )
                .map(|category| vec![category])
            },
        );
    }

    if selected.includes(CleanupCategoryArg::SharedCargoTargets) {
        isolate_cleanup_category(
            &mut categories,
            SHARED_CARGO_TARGETS_METADATA,
            apply,
            None,
            None,
            || {
                let output =
                    cleanup::cleanup_shared_cargo_targets(cleanup::CargoTargetCleanupOptions {
                        root: None,
                        apply,
                        older_than: policy.shared_store_min_age(),
                        max_bytes: policy.shared_store_max_bytes,
                        limit: policy.scan_limit(),
                        cursor: args.cursor.clone(),
                        now: std::time::SystemTime::now(),
                        lease_ttl: policy.shared_store_lease_ttl(),
                        deadline: None,
                    })?;
                category_from_output(
                    SHARED_CARGO_TARGETS_METADATA,
                    apply,
                    output.candidate_count,
                    output.applied_count,
                    output.skipped_count,
                    output.candidates.iter().map(|store| store.size_bytes).sum(),
                    output.reclaimed_bytes,
                    output,
                )
                .map(|category| vec![category])
            },
        );
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
    let applied_category_count = applied_category_count(&categories);
    let failed_category_count = categories
        .iter()
        .filter(|category| category.failure.is_some())
        .count();
    let actionable = cleanup_actionable(&categories, apply);
    let output = serde_json::to_value(CleanupInventoryOutput {
        command: "cleanup.inventory",
        status: if failed_category_count == 0 {
            "succeeded"
        } else {
            "partial_failure"
        },
        mode: if apply { "apply" } else { "dry_run" },
        category_count: categories.len(),
        failed_category_count,
        candidate_count,
        applied_count,
        skipped_count,
        applied_category_count,
        estimated_bytes,
        reclaimed_bytes,
        retention: policy,
        categories,
        actionable,
    })
    .map_err(|err| {
        homeboy::core::Error::internal_json(err.to_string(), Some("cleanup inventory".to_string()))
    })?;
    Ok(CleanupInventoryResult {
        output,
        exit_code: if failed_category_count == 0 { 0 } else { 1 },
    })
}

fn cleanup_replay_command(args: &CleanupArgs, full: bool, include_cursor: bool) -> String {
    let mut command = "homeboy cleanup".to_string();
    if !args.include.is_empty() {
        command.push_str(&format!(
            " --include {}",
            args.include
                .iter()
                .map(cleanup_category_arg_name)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if !args.exclude.is_empty() {
        command.push_str(&format!(
            " --exclude {}",
            args.exclude
                .iter()
                .map(cleanup_category_arg_name)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if args.apply {
        command.push_str(" --apply");
    }
    if let Some(days) = args.older_than_days {
        command.push_str(&format!(" --older-than-days {days}"));
    }
    if let Some(days) = args.runtime_tmp_managed_older_than_days {
        command.push_str(&format!(" --runtime-tmp-managed-older-than-days {days}"));
    }
    if let Some(limit) = args.limit {
        command.push_str(&format!(" --limit {limit}"));
    }
    if include_cursor {
        if let Some(cursor) = &args.cursor {
            command.push_str(&format!(" --cursor {}", quote_arg(cursor)));
        }
    }
    if full {
        command.push_str(" --full");
    }
    command
}

fn cleanup_category_arg_name(category: &CleanupCategoryArg) -> &'static str {
    match category {
        CleanupCategoryArg::RepoArtifacts => "repo-artifacts",
        CleanupCategoryArg::TaskWorktrees => "task-worktrees",
        CleanupCategoryArg::WorktreeProviders => "worktree-providers",
        CleanupCategoryArg::TerminalRuns => "terminal-runs",
        CleanupCategoryArg::PersistedRunArtifacts => "persisted-run-artifacts",
        CleanupCategoryArg::OrphanedArtifactBytes => "orphaned-artifact-bytes",
        CleanupCategoryArg::RunnerDownloads => "runner-downloads",
        CleanupCategoryArg::RunnerBinaryCaches => "runner-binary-caches",
        CleanupCategoryArg::RemoteLabWorkspaces => "remote-lab-workspaces",
        CleanupCategoryArg::RuntimeTmp => "runtime-tmp",
        CleanupCategoryArg::ControllerScratch => "controller-scratch",
        CleanupCategoryArg::SharedCargoTargets => "shared-cargo-targets",
        CleanupCategoryArg::ControllerRuntimes => "controller-runtimes",
    }
}

fn isolate_cleanup_category(
    categories: &mut Vec<CleanupInventoryCategory>,
    metadata: CleanupInventoryCategoryMetadata,
    apply: bool,
    canonical_cleanup_command: Option<&str>,
    specialist_command: Option<&str>,
    action: impl FnOnce() -> homeboy::core::Result<Vec<CleanupInventoryCategory>>,
) {
    match action() {
        Ok(mut completed) => categories.append(&mut completed),
        Err(error) => categories.push(cleanup_category_failure(
            metadata,
            apply,
            canonical_cleanup_command,
            specialist_command,
            error,
        )),
    }
}

fn cleanup_category_failure(
    metadata: CleanupInventoryCategoryMetadata,
    apply: bool,
    canonical_cleanup_command: Option<&str>,
    specialist_command: Option<&str>,
    error: homeboy::core::Error,
) -> CleanupInventoryCategory {
    let canonical_cleanup_command = canonical_cleanup_command
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| metadata.canonical_cleanup_command(apply));
    let specialist_command =
        specialist_command.unwrap_or_else(|| metadata.specialist_command(apply));
    // Metadata represents a family of runner-specific commands. Without an
    // observed runner ID, retry the executable aggregate command instead.
    let specialist_command = if specialist_command.contains("<runner>") {
        canonical_cleanup_command.clone()
    } else {
        specialist_command.to_string()
    };
    CleanupInventoryCategory {
        category: metadata.category,
        canonical_cleanup_command,
        specialist_command,
        included: true,
        skipped: true,
        skip_reason: Some(error.message.clone()),
        failure: Some(CleanupInventoryCategoryFailure {
            code: error.code.as_str().to_string(),
            message: error.message,
            retryable: error.retryable,
        }),
        candidate_count: 0,
        applied_count: 0,
        skipped_count: 1,
        estimated_bytes: 0,
        reclaimed_bytes: 0,
        output: error.details,
    }
}

struct CleanupCategorySelection {
    include: Vec<CleanupCategoryArg>,
    exclude: Vec<CleanupCategoryArg>,
}

/// Categories a bare `homeboy cleanup` deliberately does not sweep.
///
/// Everything else in the aggregate reclaims bytes Homeboy produced as a
/// byproduct of its own work — scratch, build targets, temp trees, crash
/// residue, remote workspaces. `runner-downloads` is different in kind: every
/// byte under `<artifact-root>/runner` is the result of a fetch an operator
/// asked for, and `homeboy runs artifact get` hands that exact path back to
/// them as the location of their file. The predicate in
/// [`homeboy::core::observation::runs_service::cleanup_runner_downloads`] proves
/// the bytes are old and unclaimed, but it cannot prove the operator is *done*
/// with them, because the single writer emits the same name shape for an
/// operator pull and for an internal auto-fetch (#10564).
///
/// Until the writer tags its output, an explicit `--include runner-downloads`
/// is the honest contract: being absent from a default sweep is cheap and
/// reversible, and a wrong delete is neither. The category stays fully visible
/// in `homeboy cleanup retained-storage`, which names the reclaim command.
const OPT_IN_ONLY_CATEGORIES: &[CleanupCategoryArg] = &[CleanupCategoryArg::RunnerDownloads];

impl CleanupCategorySelection {
    fn new(include: Vec<CleanupCategoryArg>, exclude: Vec<CleanupCategoryArg>) -> Self {
        Self { include, exclude }
    }

    fn includes(&self, category: CleanupCategoryArg) -> bool {
        let selected = if self.include.is_empty() {
            !OPT_IN_ONLY_CATEGORIES.contains(&category)
        } else {
            self.include.contains(&category)
        };
        selected && !self.exclude.contains(&category)
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
        failure: None,
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
                    min_age_days: None,
                    include_active_worktrees: false,
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
                min_age_days: None,
                include_active_worktrees: false,
            },
        ));
    }
    collection
}

/// Categories that executed and removed at least one resource.
///
/// Counted in categories, deliberately not in resources: a directory-level
/// atomic cleanup removes many resources in one operation, and folding that into
/// a resource-unit field made a complete sweep read as a partial one (#9483).
fn applied_category_count(categories: &[CleanupInventoryCategory]) -> usize {
    categories
        .iter()
        .filter(|category| category.applied_count > 0)
        .count()
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
        failure: None,
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
        failure: None,
        candidate_count: persisted.planned_record_count + resource_cleanup_candidates,
        applied_count: persisted.removed_record_count,
        skipped_count: persisted.skipped_count,
        estimated_bytes: persisted.totals.planned_size_bytes,
        reclaimed_bytes: persisted.totals.removed_size_bytes,
        output,
    })
}

fn remote_lab_workspace_categories(
    policy: CleanupPolicy,
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
                failure: None,
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
                min_age_hours: policy.runner_min_age_hours,
                // A page size, not a delete budget: `--limit` is a record
                // budget for row-driven categories and is deliberately not
                // wired here. See `cleanup::RUNNER_WORKSPACE_PAGE_LIMIT`.
                limit: policy.runner_workspace_page_limit,
                passes: CleanupPolicy::runner_workspace_passes(apply),
                cursor: None,
                ..RunnerWorkspacePruneOptions::default()
            },
        ) {
            Ok((output, _)) => output,
            Err(error) => {
                categories.push(cleanup_category_failure(
                    REMOTE_LAB_WORKSPACES_METADATA,
                    apply,
                    None,
                    Some(&runner_workspace_specialist_command(
                        &status.runner_id,
                        apply,
                    )),
                    error,
                ));
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
    let command = runner_workspace_specialist_command(&output.runner_id, apply);
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

fn runner_workspace_specialist_command(runner_id: &str, apply: bool) -> String {
    if apply {
        format!(
            "homeboy runner workspace prune {} --apply --passes 10",
            quote_arg(runner_id)
        )
    } else {
        format!("homeboy runner workspace prune {}", quote_arg(runner_id))
    }
}

fn runner_binary_cache_categories(
    policy: CleanupPolicy,
    apply: bool,
) -> homeboy::core::Result<Vec<CleanupInventoryCategory>> {
    runner::list()?
        .into_iter()
        .map(|configured| runner_binary_cache_category(policy, &configured.id, apply))
        .collect()
}

fn runner_binary_cache_category(
    policy: CleanupPolicy,
    runner_id: &str,
    apply: bool,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    let specialist_command = format!(
        "homeboy runner cache-prune {}{}",
        quote_arg(runner_id),
        if apply { " --apply" } else { "" }
    );
    let output = match runner::prune_homeboy_binary_cache(
        runner_id,
        RunnerBinaryCachePruneOptions {
            apply,
            min_age_hours: policy.runner_min_age_hours,
        },
    ) {
        Ok((output, _)) => output,
        Err(error) => {
            return Ok(cleanup_category_failure(
                RUNNER_BINARY_CACHES_METADATA,
                apply,
                None,
                Some(&specialist_command),
                error,
            ));
        }
    };
    runner_binary_cache_output_category(output, apply, specialist_command)
}

fn runner_binary_cache_output_category(
    output: RunnerBinaryCachePruneOutput,
    apply: bool,
    specialist_command: String,
) -> homeboy::core::Result<CleanupInventoryCategory> {
    category_from_command(
        RUNNER_BINARY_CACHES_METADATA.category,
        RUNNER_BINARY_CACHES_METADATA.canonical_cleanup_command(apply),
        specialist_command,
        output.eligible.len(),
        output.removed.len(),
        output.skipped.len(),
        output.eligible_bytes,
        output.removed_bytes,
        output,
    )
}

fn cleanup_actionable(
    categories: &[CleanupInventoryCategory],
    apply: bool,
) -> CommandActionableMetadata {
    let mut actionable = CommandActionableMetadata::default();
    for category in categories {
        if category.failure.is_some() {
            actionable.next_actions.push(
                CommandNextAction::new(
                    format!("retry {} cleanup", category.category.replace('_', " ")),
                    category.specialist_command.clone(),
                )
                .with_kind(CommandNextActionKind::Repair),
            );
            continue;
        }
        if category.skipped || category.candidate_count == 0 {
            continue;
        }
        actionable.next_actions.push(CommandNextAction::new(
            format!("{} cleanup", category.category.replace('_', " ")),
            if category.category == REPO_ARTIFACTS_METADATA.category {
                apply_command(&category.canonical_cleanup_command)
            } else if apply {
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
    let remaining_count = payload
        .get("remaining_count")
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
    let mut lines = vec![
        "Artifact cleanup summary".to_string(),
        format!(
            "Mode: {}",
            if mode == "apply" { "apply" } else { "dry run" }
        ),
        format!("Root: {root}"),
        format!("Candidates: {candidate_count}"),
        format!("Applied: {applied_count}"),
        format!("Remaining candidates: {remaining_count}"),
        format!("Estimated reclaimable: {}", format_bytes(estimated_bytes)),
        format!(
            "Estimated reclaimable (allocated): {}",
            format_bytes(allocated_total(payload, "estimated_allocated_bytes"))
        ),
        format!("Reclaimed: {}", format_bytes(reclaimed_bytes)),
        format!(
            "Reclaimed (allocated): {}",
            format_bytes(allocated_total(payload, "reclaimed_allocated_bytes"))
        ),
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

    let rehydrate_commands = rehydrate_command_lines(payload);
    if !rehydrate_commands.is_empty() {
        lines.push("Rehydrate removed dependencies with:".to_string());
        lines.extend(
            rehydrate_commands
                .into_iter()
                .map(|command| format!("  - {command}")),
        );
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
            if let Some(outcome) = provider.get("outcome").and_then(Value::as_str) {
                lines.push(format!("  Outcome: {outcome}"));
            }
            if let Some(completeness) = provider
                .get("inventory_completeness")
                .and_then(Value::as_str)
            {
                lines.push(format!("  Inventory: {completeness}"));
            }
            if let Some(elapsed_ms) = provider.get("elapsed_ms").and_then(Value::as_u64) {
                lines.push(format!("  Elapsed: {elapsed_ms} ms"));
            }
            if let Some(heartbeat_count) = provider.get("heartbeat_count").and_then(Value::as_u64) {
                lines.push(format!("  Heartbeats: {heartbeat_count}"));
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

/// Allocated-byte totals are optional in the payload so an older cached report
/// still renders; a missing total reads as zero rather than hiding the line.
fn allocated_total(payload: &Value, field: &str) -> u64 {
    payload.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// Deduplicated rehydration guidance across every inspected checkout. Operators
/// need the set of commands that restores what cleanup removed, not one line
/// per removed path.
fn rehydrate_command_lines(payload: &Value) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    for command in payload
        .get("worktrees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|worktree| worktree.get("rehydrate_commands").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
    {
        if !commands.iter().any(|existing| existing == command) {
            commands.push(command.to_string());
        }
    }
    commands
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
mod count_unit_tests {
    use super::*;

    fn category(name: &'static str, candidates: usize, applied: usize) -> CleanupInventoryCategory {
        category_from_command(
            name,
            format!("homeboy cleanup --include {name} --apply"),
            format!("homeboy {name} --apply"),
            candidates,
            applied,
            0,
            0,
            0,
            serde_json::json!({}),
        )
        .expect("category fixture")
    }

    /// A directory-level atomic sweep removes every resource it selected. That
    /// must read as complete, not as 1-of-588 (#9483).
    #[test]
    fn runner_downloads_style_atomic_cleanup_reports_matching_resource_counts() {
        let categories = vec![category("runner-downloads", 588, 588)];

        assert_eq!(categories[0].candidate_count, 588);
        assert_eq!(categories[0].applied_count, 588);
        assert_eq!(
            applied_category_count(&categories),
            1,
            "one category executed, regardless of how many resources it removed"
        );
    }

    /// Category-level success and resource-level counts stay in separate units
    /// across runner downloads, runtime temp, and terminal runs.
    #[test]
    fn category_success_is_counted_in_categories_not_resources() {
        let categories = vec![
            category("runner-downloads", 588, 588),
            category("runtime-temp", 12, 12),
            category("terminal-runs", 4, 0),
        ];

        let candidate_total: usize = categories.iter().map(|c| c.candidate_count).sum();
        let applied_total: usize = categories.iter().map(|c| c.applied_count).sum();

        assert_eq!(candidate_total, 604);
        assert_eq!(applied_total, 600);
        assert_eq!(
            applied_category_count(&categories),
            2,
            "terminal-runs removed nothing, so only two categories executed"
        );
    }

    #[test]
    fn a_dry_run_with_candidates_has_no_applied_categories() {
        let categories = vec![
            category("runner-downloads", 588, 0),
            category("runtime-temp", 12, 0),
        ];

        assert_eq!(applied_category_count(&categories), 0);
    }
}

#[cfg(test)]
mod tests {
    use homeboy::runner::runners::{RunnerActiveJobState, RunnerSessionState, RunnerStatusReport};
    use serde_json::json;
    use std::process::Command;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn runtime_temp_retained_records_expose_producer_run_and_unknown_boundaries() {
        let managed = runtime_tmp_retained_record(engine::temp::RuntimeTempCleanupRow {
            path: "/tmp/managed".to_string(),
            name: "managed".to_string(),
            action: "skip".to_string(),
            reason: "runtime temp pin owner PID 1 is running".to_string(),
            size_bytes: 42,
            allocated_bytes: 42,
            verified_reclaimed_bytes: 0,
            owner_id: Some("owner-1".to_string()),
            owner_pid: Some(1),
            owner_state: Some("active".to_string()),
            producer: Some("runner_execution".to_string()),
            run_id: Some("run-1".to_string()),
            age_seconds: Some(1),
            protection_reason: Some("runtime temp pin owner PID 1 is running".to_string()),
        });
        assert_eq!(managed.owner, "runner_execution (owner-1)");
        assert_eq!(managed.run_id.as_deref(), Some("run-1"));
        assert_eq!(managed.liveness, "active");
        assert!(managed
            .reason
            .contains("homeboy cleanup --include runtime-tmp"));

        let unknown = runtime_tmp_retained_record(engine::temp::RuntimeTempCleanupRow {
            path: "/tmp/external".to_string(),
            name: "external".to_string(),
            action: "skip".to_string(),
            reason: "entry is newer than retention cutoff".to_string(),
            size_bytes: 11,
            allocated_bytes: 11,
            verified_reclaimed_bytes: 0,
            owner_id: None,
            owner_pid: None,
            owner_state: None,
            producer: None,
            run_id: None,
            age_seconds: None,
            protection_reason: None,
        });
        assert_eq!(unknown.owner, "external/unattributed");
        assert_eq!(unknown.liveness, "external_unknown");
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
            test_filesystem(),
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
            test_filesystem(),
        );
        assert_eq!(continuation.largest_examples[0].reference, "runtime-a");
        assert_eq!(age_bucket(3_600), "under_1d");
        // No existing producer emits `reclaimable`, so the split is inert until
        // an artifact-root producer contributes.
        assert_eq!(report.reclaimable_count, 0);
        assert_eq!(report.reclaimable_bytes, 0);
    }

    fn retained_record(category: &str, liveness: &str, size_bytes: u64) -> RetainedStorageRecord {
        RetainedStorageRecord {
            category: category.to_string(),
            reason: "test".to_string(),
            owner: "homeboy".to_string(),
            run_id: None,
            liveness: liveness.to_string(),
            age: "unknown".to_string(),
            age_seconds: None,
            size_bytes,
            reference: format!("{category}/{liveness}"),
        }
    }

    fn test_sqlite() -> RetainedStorageSqlite {
        RetainedStorageSqlite {
            path: "homeboy.sqlite".to_string(),
            exists: true,
            size_bytes: 10,
            status_command: "homeboy db status",
            compaction: "delegated",
        }
    }

    fn test_filesystem() -> RetainedStorageFilesystem {
        RetainedStorageFilesystem {
            root: RetainedStorageFilesystemUsage {
                path: "/homeboy".to_string(),
                exists: true,
                apparent_bytes: 0,
                physical_bytes: 0,
            },
            top_level: Vec::new(),
            reconciliation: RetainedStorageReconciliation {
                top_level_apparent_bytes: 0,
                top_level_physical_bytes: 0,
                apparent_difference_bytes: 0,
                physical_difference_bytes: 0,
                apparent_difference_direction: "equal",
                physical_difference_direction: "equal",
            },
            accounting_notes: Vec::new(),
        }
    }

    #[test]
    fn filesystem_inventory_exposes_large_cargo_children_and_unknown_storage() {
        let root = TempDir::new().expect("storage root");
        let cargo = root.path().join(homeboy::core::paths::CARGO_TARGETS_STORE);
        let target = cargo.join("homeboy-b706ed1ffed4");
        std::fs::create_dir_all(&target).expect("cargo target");
        let debug = target.join("debug");
        std::fs::File::create(&debug)
            .expect("debug artifact")
            .set_len(42 * 1024 * 1024 * 1024)
            .expect("sparse 42 GiB debug artifact");
        let unknown = root.path().join("left-behind-store");
        std::fs::create_dir_all(&unknown).expect("unknown store");
        std::fs::write(unknown.join("data"), b"unmanaged").expect("unknown data");

        let cargo_entry = filesystem_entry(cargo).expect("cargo inventory");
        assert_eq!(cargo_entry.category, "shared_cargo_targets");
        assert_eq!(
            cargo_entry.cleanup_or_status_command,
            "homeboy cleanup --include shared-cargo-targets"
        );
        assert!(
            cargo_entry.largest_examples[0].apparent_bytes >= 42 * 1024 * 1024 * 1024,
            "the target directory includes its own metadata beside the debug file"
        );
        assert!(cargo_entry.largest_examples[0]
            .path
            .ends_with("homeboy-b706ed1ffed4"));

        let unknown_entry = filesystem_entry(unknown).expect("unknown inventory");
        assert_eq!(unknown_entry.category, "unknown_storage");
        assert_eq!(unknown_entry.classification, "unknown/unmanaged");
        assert_eq!(
            unknown_entry.cleanup_or_status_command,
            "inspect path ownership before removal"
        );
    }

    #[test]
    fn retained_storage_filesystem_inventory_reports_configured_external_cargo_root() {
        let data = TempDir::new().expect("data root");
        let cargo_root = TempDir::new().expect("cargo root");
        std::fs::write(cargo_root.path().join("artifact"), b"cargo output")
            .expect("cargo artifact");

        let inventory = retained_storage_filesystem_inventory_for(
            data.path().to_path_buf(),
            data.path().join("artifacts"),
            cargo_root.path().to_path_buf(),
        )
        .expect("filesystem inventory");

        let cargo = inventory
            .top_level
            .iter()
            .find(|entry| entry.category == "shared_cargo_targets")
            .expect("configured Cargo root");
        assert_eq!(cargo.path, cargo_root.path().display().to_string());
        assert_eq!(cargo.classification, "managed/external");
        assert_eq!(
            cargo.cleanup_or_status_command,
            "homeboy cleanup --include shared-cargo-targets"
        );
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_usage_deduplicates_hard_links_in_root_totals() {
        let root = TempDir::new().expect("storage root");
        let first = root.path().join("cargo-targets");
        let second = root.path().join("controller-runtimes");
        std::fs::create_dir_all(&first).expect("first store");
        std::fs::create_dir_all(&second).expect("second store");
        let original = first.join("shared");
        std::fs::File::create(&original)
            .expect("shared file")
            .set_len(1024 * 1024)
            .expect("shared file length");
        std::fs::hard_link(&original, second.join("shared")).expect("hard link");

        let root_usage = filesystem_usage(root.path()).expect("root usage");
        let per_store_apparent = filesystem_usage(&first)
            .expect("first usage")
            .apparent_bytes
            + filesystem_usage(&second)
                .expect("second usage")
                .apparent_bytes;
        assert!(
            root_usage.apparent_bytes < per_store_apparent,
            "root accounting must count one shared inode once"
        );
    }

    #[test]
    fn reclaimable_storage_is_never_summed_into_the_retained_total() {
        // "cleanup cannot free this" and "cleanup has not freed this yet" are
        // different answers to "where did my disk go". Adding them together
        // would tell an operator their disk is unreclaimable when it is not.
        let report = build_retained_storage_report(
            vec![
                retained_record("shared_cargo_targets", "active", 100),
                retained_record("persisted_run_artifacts", LIVENESS_RECLAIMABLE, 900),
                retained_record("runner_downloads", LIVENESS_RECLAIMABLE, 50),
            ],
            10,
            None,
            test_sqlite(),
            test_filesystem(),
        );

        assert_eq!(report.inspected_count, 3);
        assert_eq!(report.retained_count, 1);
        assert_eq!(report.retained_bytes, 100);
        assert_eq!(report.reclaimable_count, 2);
        assert_eq!(report.reclaimable_bytes, 950);
        // Both remain visible in the dimensional aggregates, which cover every
        // inspected record.
        assert!(report
            .by_liveness
            .iter()
            .any(|row| row.key == LIVENESS_RECLAIMABLE && row.size_bytes == 950));
    }

    #[test]
    fn retained_storage_names_a_reclaim_command_for_every_artifact_root_category() {
        let report =
            build_retained_storage_report(Vec::new(), 1, None, test_sqlite(), test_filesystem());

        // The report used to accumulate from five sources and never reach the
        // artifact root, so an operator saw bytes it could not name a command
        // for (#10316).
        for category in [
            "persisted-run-artifacts",
            "runner-downloads",
            "orphaned-artifact-bytes",
        ] {
            assert!(
                report
                    .safe_next_commands
                    .iter()
                    .any(|command| command == &format!("homeboy cleanup --include {category}")),
                "missing reclaim command for {category}"
            );
        }
    }

    #[test]
    fn automatic_retention_and_retained_storage_keep_their_store_ownership_boundary() {
        // #10738 applies bounded lifecycle cleanup for controller runtimes, while
        // #9824 only reports filesystem attribution and names the same owner.
        // Cargo remains a separate budgeted automatic pass, so it must not enter
        // the aggregate category sweep.
        assert!(AUTOMATIC_RETENTION_CATEGORIES.contains(&CleanupCategoryArg::ControllerRuntimes));
        assert!(!AUTOMATIC_RETENTION_CATEGORIES.contains(&CleanupCategoryArg::SharedCargoTargets));

        let report =
            build_retained_storage_report(Vec::new(), 1, None, test_sqlite(), test_filesystem());
        for command in [
            "homeboy cleanup --include controller-runtimes",
            "homeboy cleanup --include shared-cargo-targets",
        ] {
            assert!(
                report
                    .safe_next_commands
                    .iter()
                    .any(|entry| entry == command),
                "retained-storage must preserve the owning command for {command}"
            );
        }
    }

    #[test]
    fn aggregate_and_specialist_share_one_runner_age_floor() {
        // The aggregate carried a literal `24` beside each specialist's
        // `default_value_t = 24`. One named constant is now the only source.
        assert_eq!(cleanup::RUNNER_MIN_AGE_HOURS, 24);
        let policy = cleanup::cleanup_policy_from_retention(
            &defaults::RetentionConfig::default(),
            CleanupPolicyOverrides::default(),
        )
        .expect("resolve policy");
        assert_eq!(policy.runner_min_age_hours, cleanup::RUNNER_MIN_AGE_HOURS);
        assert_eq!(
            policy.runner_workspace_page_limit,
            cleanup::RUNNER_WORKSPACE_PAGE_LIMIT
        );
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
            // #10564: opt-in-only categories are absent from a bare sweep,
            // reachable by an explicit `--include`, and still suppressible by
            // `--exclude`.
            (vec![], vec![], CleanupCategoryArg::RunnerDownloads, false),
            (
                vec![CleanupCategoryArg::RunnerDownloads],
                vec![],
                CleanupCategoryArg::RunnerDownloads,
                true,
            ),
            (
                vec![CleanupCategoryArg::RunnerDownloads],
                vec![CleanupCategoryArg::RunnerDownloads],
                CleanupCategoryArg::RunnerDownloads,
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
    fn runtime_tmp_failure_does_not_prevent_later_independent_categories() {
        let mut categories = Vec::new();
        let mut executed = Vec::new();

        isolate_cleanup_category(
            &mut categories,
            RUNTIME_TMP_METADATA,
            true,
            Some(
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 3 --apply",
            ),
            Some(
                "homeboy self cleanup-runtime-tmp --runtime-tmp-managed-older-than-days 3 --apply",
            ),
            || {
            executed.push("runtime_tmp");
            Err(homeboy::core::Error::internal_io(
                "Permission denied (os error 13)",
                Some("read runtime temp directory".to_string()),
            ))
            },
        );
        isolate_cleanup_category(
            &mut categories,
            CONTROLLER_SCRATCH_METADATA,
            true,
            None,
            None,
            || {
                executed.push("controller_scratch");
                category_from_output(
                    CONTROLLER_SCRATCH_METADATA,
                    true,
                    1,
                    1,
                    0,
                    128,
                    128,
                    serde_json::json!({ "reclaimed": "scratch" }),
                )
                .map(|category| vec![category])
            },
        );
        isolate_cleanup_category(
            &mut categories,
            CONTROLLER_RUNTIMES_METADATA,
            true,
            None,
            None,
            || {
                executed.push("controller_runtimes");
                category_from_output(
                    CONTROLLER_RUNTIMES_METADATA,
                    true,
                    1,
                    1,
                    0,
                    256,
                    256,
                    serde_json::json!({ "reclaimed": "runtimes" }),
                )
                .map(|category| vec![category])
            },
        );

        assert_eq!(
            executed,
            ["runtime_tmp", "controller_scratch", "controller_runtimes"]
        );
        assert_eq!(categories.len(), 3);
        assert_eq!(categories[0].category, "runtime_tmp");
        assert_eq!(
            categories[0]
                .failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            Some("internal.io_error")
        );
        assert_eq!(categories[1].reclaimed_bytes, 128);
        assert_eq!(categories[2].reclaimed_bytes, 256);
        assert_eq!(
            categories[0].canonical_cleanup_command,
            "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 3 --apply"
        );

        let actionable = cleanup_actionable(&categories, true);
        assert_eq!(
            actionable
                .next_actions
                .iter()
                .find(|action| action.kind.is_some())
                .map(|action| action.command.as_str()),
            Some(
                "homeboy self cleanup-runtime-tmp --runtime-tmp-managed-older-than-days 3 --apply"
            )
        );
    }

    #[test]
    fn aggregate_runtime_tmp_failure_returns_partial_json_and_continues() {
        homeboy::test_support::with_isolated_home(|root| {
            let blocked_root = root.path().join("runtime-tmp-file");
            std::fs::write(&blocked_root, "not a directory").expect("runtime temp fixture");
            let previous = std::env::var_os("HOMEBOY_RUNTIME_TMPDIR");
            std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", &blocked_root);

            let result = run(CleanupArgs {
                apply: true,
                include: vec![
                    CleanupCategoryArg::RuntimeTmp,
                    CleanupCategoryArg::ControllerScratch,
                    CleanupCategoryArg::ControllerRuntimes,
                ],
                exclude: Vec::new(),
                older_than_days: None,
                runtime_tmp_managed_older_than_days: Some(3),
                limit: Some(10),
                full: false,
                cursor: None,
                command: None,
            });

            match previous {
                Some(value) => std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", value),
                None => std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR"),
            }

            let (output, exit_code) = result.expect("aggregate result");
            assert_eq!(exit_code, 1);
            assert_eq!(output["status"], "partial_failure");
            assert_eq!(output["failed_category_count"], 1);
            let categories = output["categories"].as_array().expect("categories");
            assert_eq!(categories.len(), 3);
            assert_eq!(categories[0]["category"], "runtime_tmp");
            assert_eq!(categories[0]["failure"]["code"], "internal.io_error");
            assert_eq!(
                categories[0]["canonical_cleanup_command"],
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 3 --apply"
            );
            assert_eq!(categories[1]["category"], "controller_scratch");
            assert!(categories[1]["failure"].is_null());
            assert_eq!(categories[2]["category"], "controller_runtimes");
            assert!(categories[2]["failure"].is_null());
        });
    }

    #[test]
    fn automatic_retention_propagates_aggregate_partial_failure() {
        homeboy::test_support::with_isolated_home(|root| {
            let blocked_root = root.path().join("runtime-tmp-file");
            std::fs::write(&blocked_root, "not a directory").expect("runtime temp fixture");
            let previous = std::env::var_os("HOMEBOY_RUNTIME_TMPDIR");
            std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", &blocked_root);

            let result = automatic_retention();

            match previous {
                Some(value) => std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", value),
                None => std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR"),
            }

            let (output, exit_code) = result.expect("automatic retention result");
            assert_eq!(exit_code, 1);
            assert_eq!(output["status"], "partial_failure");
            assert_eq!(output["cleanup"]["categories"]["status"], "partial_failure");
        });
    }

    #[test]
    fn runner_category_failures_are_typed_and_never_retry_a_placeholder() {
        let category = cleanup_category_failure(
            REMOTE_LAB_WORKSPACES_METADATA,
            true,
            None,
            None,
            homeboy::core::Error::internal_io("runner unavailable", None),
        );

        assert_eq!(
            category
                .failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            Some("internal.io_error")
        );
        assert_eq!(
            category.specialist_command,
            "homeboy cleanup --include remote-lab-workspaces --apply"
        );
        assert!(!category.specialist_command.contains("<runner>"));
    }

    #[test]
    fn only_runner_downloads_is_withheld_from_the_bare_sweep() {
        // A bare `homeboy cleanup --apply` must keep sweeping everything that
        // reclaims Homeboy's own byproducts. Only the operator-owned download
        // cache is withheld, and the withheld set is asserted exactly so a
        // future category cannot be quietly dropped from the default (#10564).
        assert_eq!(
            OPT_IN_ONLY_CATEGORIES.to_vec(),
            vec![CleanupCategoryArg::RunnerDownloads]
        );

        let bare = CleanupCategorySelection::new(Vec::new(), Vec::new());
        for category in [
            CleanupCategoryArg::RepoArtifacts,
            CleanupCategoryArg::TaskWorktrees,
            CleanupCategoryArg::WorktreeProviders,
            CleanupCategoryArg::TerminalRuns,
            CleanupCategoryArg::PersistedRunArtifacts,
            CleanupCategoryArg::OrphanedArtifactBytes,
            CleanupCategoryArg::RunnerBinaryCaches,
            CleanupCategoryArg::RemoteLabWorkspaces,
            CleanupCategoryArg::RuntimeTmp,
            CleanupCategoryArg::ControllerScratch,
            CleanupCategoryArg::SharedCargoTargets,
            CleanupCategoryArg::ControllerRuntimes,
        ] {
            assert!(
                bare.includes(category),
                "bare cleanup must still sweep {category:?}"
            );
        }
        assert!(!bare.includes(CleanupCategoryArg::RunnerDownloads));
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
        std::fs::write(repository.path().join(".gitignore"), "target/\n")
            .expect("target ignore rule");
        Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(repository.path())
            .output()
            .expect("stage ignore rule");
        Command::new("git")
            .args([
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(repository.path())
            .output()
            .expect("commit ignore rule");
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
                "homeboy worktree cleanup --cleanup-branches",
                "homeboy worktree cleanup --cleanup-branches --apply",
            ),
            (
                TERMINAL_RUNS_METADATA,
                "terminal_runs",
                "terminal-runs",
                "homeboy cleanup --include terminal-runs",
                "homeboy cleanup --include terminal-runs --apply",
            ),
            (
                PERSISTED_RUN_ARTIFACTS_METADATA,
                "persisted_run_artifacts",
                "persisted-run-artifacts",
                "homeboy runs artifact cleanup-persisted",
                "homeboy runs artifact cleanup-persisted --apply",
            ),
            (
                ORPHANED_ARTIFACT_BYTES_METADATA,
                "orphaned_artifact_bytes",
                "orphaned-artifact-bytes",
                "homeboy cleanup --include orphaned-artifact-bytes",
                "homeboy cleanup --include orphaned-artifact-bytes --apply",
            ),
            (
                RUNNER_DOWNLOADS_METADATA,
                "runner_downloads",
                "runner-downloads",
                "homeboy runs artifact cleanup-downloads",
                "homeboy runs artifact cleanup-downloads --apply",
            ),
            (
                RUNNER_BINARY_CACHES_METADATA,
                "runner_binary_caches",
                "runner-binary-caches",
                "homeboy runner cache-prune <runner>",
                "homeboy runner cache-prune <runner> --apply",
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
                "homeboy runtime controller-prune",
                "homeboy runtime controller-prune --apply",
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
    fn managed_runtime_tmp_override_is_preserved_in_followup_commands() {
        assert_eq!(
            runtime_tmp_commands(false, 0),
            (
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 0"
                    .to_string(),
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 0"
                    .to_string(),
            )
        );
        assert_eq!(
            runtime_tmp_commands(true, 1),
            (
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 1 --apply"
                    .to_string(),
                "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 1 --apply"
                    .to_string(),
            )
        );
    }

    #[test]
    fn task_worktree_cleanup_next_actions_preserve_mode_specific_commands() {
        let cases = [
            (false, "homeboy worktree cleanup --cleanup-branches --apply"),
            (true, "homeboy worktree cleanup --cleanup-branches --apply"),
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
                failure: None,
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
            "remaining_count": 3,
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
    fn cleanup_artifacts_apply_summary_uses_post_apply_remaining_count() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "apply",
            "root": "/tmp/homeboy",
            "candidate_count": 4,
            "skipped_count": 1,
            "applied_count": 3,
            "remaining_count": 0,
            "estimated_bytes": 4096,
            "reclaimed_bytes": 3072,
            "skipped": [
                { "reason": "worktree branch is not merged into its upstream" }
            ]
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Mode: apply\n"));
        assert!(summary.contains("Remaining candidates: 0\n"));
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
            "remaining_count": 2,
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
            "remaining_count": 12,
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
                        "outcome": "completed",
                        "inventory_completeness": "complete",
                        "elapsed_ms": 250,
                        "heartbeat_count": 2,
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
        assert!(summary.contains("  Outcome: completed\n"));
        assert!(summary.contains("  Inventory: complete\n"));
        assert!(summary.contains("  Elapsed: 250 ms\n"));
        assert!(summary.contains("  Heartbeats: 2\n"));
        assert!(summary.contains("  Last observed progress: removed 10/20\n"));
        assert!(summary.contains("  Run: cleanup-run-1\n"));
        assert!(summary.contains("  Status command: provider status cleanup-run-1\n"));
        assert!(summary.contains("  Safe follow-up command: provider status cleanup-run-1\n"));
    }
}
