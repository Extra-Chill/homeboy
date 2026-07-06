use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use homeboy::core::observation::{ObservationStore, RunListFilter, RunStatus};
use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::resource_lifecycle_index::{
    resource_lifecycle_index_from_artifacts, resource_lifecycle_record_is_actionable,
    resource_lifecycle_record_is_cleanup_eligible,
    transition_preserved_runner_workspaces_to_cleanup_pending, ResourceCleanupPolicy,
    ResourceEvidenceRetention, ResourceLifecycle, ResourceLifecycleCleanupOperation,
    ResourceLifecycleIndex, ResourceLifecycleRecord, ResourceLifecycleResourceStatus,
    RESOURCE_LIFECYCLE_INDEX_SCHEMA,
};
use homeboy::core::{Error, Result};
use serde::{Deserialize, Serialize};

use super::{CmdResult, RunsOutput};

#[derive(Args, Clone)]
pub struct RunsResourcesArgs {
    /// Resource lifecycle index JSON file. Repeatable. Defaults to the local observation store.
    #[arg(long = "file", value_name = "PATH")]
    pub file: Vec<PathBuf>,

    /// Emit a contract-valid sample index instead of reading files or the observation store.
    #[arg(long, conflicts_with = "file")]
    pub sample: bool,

    /// Include only resources owned by this run id.
    #[arg(long = "run-id")]
    pub run_id: Option<String>,

    /// Include only resources owned by this resource owner.
    #[arg(long)]
    pub owner: Option<String>,

    /// Include only records requiring operator/orchestrator attention.
    #[arg(long)]
    pub actionable: bool,

    /// Include only records eligible for cleanup orchestration.
    #[arg(long = "cleanup-eligible")]
    pub cleanup_eligible: bool,

    /// Emit cleanup planning data for matching resources. This is read-only unless --apply is also passed.
    #[arg(long = "cleanup-plan")]
    pub cleanup_plan: bool,

    /// Delete apply-intended cleanup-eligible resources. Requires --cleanup-root and remains bounded by --limit.
    #[arg(long, requires = "cleanup_root")]
    pub apply: bool,

    /// Root directory that cleanup candidates must canonicalize under before apply can delete them.
    #[arg(long = "cleanup-root", value_name = "PATH")]
    pub cleanup_root: Option<PathBuf>,

    /// Maximum cleanup candidates to include in the plan/apply page.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Cleanup operation used by apply. Delete removes files, symlinks, or directories.
    #[arg(long = "cleanup-operation", default_value_t = ResourceCleanupOperation::Delete)]
    pub cleanup_operation: ResourceCleanupOperation,
}

impl Default for RunsResourcesArgs {
    fn default() -> Self {
        Self {
            file: Vec::new(),
            sample: false,
            run_id: None,
            owner: None,
            actionable: false,
            cleanup_eligible: false,
            cleanup_plan: false,
            apply: false,
            cleanup_root: None,
            limit: 20,
            cleanup_operation: ResourceCleanupOperation::Delete,
        }
    }
}

pub type ResourceCleanupOperation = ResourceLifecycleCleanupOperation;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesOutput {
    pub command: &'static str,
    pub schema: &'static str,
    pub mode: ResourceCleanupMode,
    pub source_count: usize,
    pub resource_count: usize,
    pub actionable_count: usize,
    pub cleanup_eligible_count: usize,
    pub sources: Vec<RunsResourcesSourceOutput>,
    pub resources: Vec<ResourceLifecycleRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<RunsResourcesCleanupOutput>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCleanupMode {
    Inspect,
    DryRun,
    Apply,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesCleanupOutput {
    pub operation: ResourceCleanupOperation,
    pub root: Option<String>,
    pub limit: usize,
    pub candidate_count: usize,
    pub planned_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub deferred_count: usize,
    pub planned: Vec<RunsResourcesCleanupPlanItem>,
    pub applied: Vec<RunsResourcesCleanupAppliedItem>,
    pub skipped: Vec<RunsResourcesCleanupSkippedItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunsResourcesCleanupPlanItem {
    pub owner: String,
    pub run_id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_bound: Option<String>,
    pub kind: String,
    pub operation: ResourceCleanupOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesCleanupAppliedItem {
    pub owner: String,
    pub run_id: String,
    pub path: String,
    pub kind: String,
    pub operation: ResourceCleanupOperation,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesCleanupSkippedItem {
    pub owner: String,
    pub run_id: String,
    pub path: String,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesSourceOutput {
    pub source: String,
    pub resource_count: usize,
}

pub fn runs_resources(args: RunsResourcesArgs) -> CmdResult<RunsOutput> {
    validate_cleanup_args(&args)?;

    let loaded = load_indexes(&args)?;
    let source_count = loaded.len();
    let mut sources = Vec::with_capacity(source_count);
    let mut all_resources = Vec::new();

    for loaded_index in loaded {
        sources.push(RunsResourcesSourceOutput {
            source: loaded_index.source,
            resource_count: loaded_index.index.resources.len(),
        });
        all_resources.extend(loaded_index.index.resources);
    }

    let actionable_count = all_resources
        .iter()
        .filter(|record| resource_lifecycle_record_is_actionable(record))
        .count();
    let cleanup_eligible_count = all_resources
        .iter()
        .filter(|record| resource_lifecycle_record_is_cleanup_eligible(record))
        .count();

    let resources = all_resources
        .into_iter()
        .filter(|record| resource_matches_filters(record, &args))
        .collect::<Vec<_>>();
    let cleanup = if args.cleanup_plan || args.apply {
        Some(build_cleanup_output(&resources, &args)?)
    } else {
        None
    };
    let mode = if args.apply {
        ResourceCleanupMode::Apply
    } else if args.cleanup_plan {
        ResourceCleanupMode::DryRun
    } else {
        ResourceCleanupMode::Inspect
    };

    Ok((
        RunsOutput::Resources(RunsResourcesOutput {
            command: "runs.resources",
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA,
            mode,
            source_count,
            resource_count: resources.len(),
            actionable_count,
            cleanup_eligible_count,
            sources,
            resources,
            cleanup,
        }),
        0,
    ))
}

fn validate_cleanup_args(args: &RunsResourcesArgs) -> Result<()> {
    if args.apply && args.sample {
        return Err(Error::validation_invalid_argument(
            "apply",
            "cleanup apply cannot run against sample data",
            None,
            Some(vec![
                "Run without --sample and pass explicit --file resource lifecycle index paths."
                    .to_string(),
            ]),
        ));
    }

    if args.apply && args.limit == 0 {
        return Err(Error::validation_invalid_argument(
            "limit",
            "cleanup apply requires a positive bounded limit",
            Some(args.limit.to_string()),
            None,
        ));
    }

    if args.apply {
        let root = args
            .cleanup_root
            .as_ref()
            .expect("clap requires cleanup_root");
        let canonical_root = canonical_cleanup_root(root)?;
        if canonical_root.parent().is_none() {
            return Err(Error::validation_invalid_argument(
                "cleanup-root",
                "cleanup root must not be the filesystem root",
                Some(root.display().to_string()),
                None,
            ));
        }
    }

    Ok(())
}

fn build_cleanup_output(
    resources: &[ResourceLifecycleRecord],
    args: &RunsResourcesArgs,
) -> Result<RunsResourcesCleanupOutput> {
    let root = match &args.cleanup_root {
        Some(path) => Some(canonical_cleanup_root(path)?),
        None => None,
    };
    let root_display = root.as_ref().map(|path| path.display().to_string());
    let limit = args.limit;
    let mut candidate_count = 0;
    let mut deferred_count = 0;
    let mut planned = Vec::new();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for record in resources {
        if !resource_lifecycle_record_is_cleanup_eligible(record) {
            skipped.push(cleanup_skip(record, "resource is not cleanup eligible"));
            continue;
        }

        candidate_count += 1;
        if planned.len() + applied.len() >= limit {
            deferred_count += 1;
            continue;
        }

        if args.apply && !record.cleanup_intent.is_apply() {
            skipped.push(cleanup_skip(
                record,
                "resource cleanup intent is dry_run; apply requires explicit apply intent",
            ));
            continue;
        }

        let Some(root) = &root else {
            planned.push(cleanup_plan_item(record, args.cleanup_operation));
            continue;
        };

        match cleanup_dispatch(record, root, args)? {
            CleanupDispatch::Plan => {
                planned.push(cleanup_plan_item(record, args.cleanup_operation))
            }
            CleanupDispatch::Applied => {
                applied.push(cleanup_applied_item(record, args.cleanup_operation))
            }
            CleanupDispatch::Skipped(reason) => skipped.push(cleanup_skip(record, reason)),
        }
    }

    Ok(RunsResourcesCleanupOutput {
        operation: args.cleanup_operation,
        root: root_display,
        limit,
        candidate_count,
        planned_count: planned.len(),
        applied_count: applied.len(),
        skipped_count: skipped.len(),
        deferred_count,
        planned,
        applied,
        skipped,
    })
}

enum CleanupDispatch {
    Plan,
    Applied,
    Skipped(String),
}

fn cleanup_dispatch(
    record: &ResourceLifecycleRecord,
    root: &Path,
    args: &RunsResourcesArgs,
) -> Result<CleanupDispatch> {
    if record.is_runner_workspace() && record.runner_id.is_some() {
        if !args.apply {
            return Ok(CleanupDispatch::Plan);
        }
        let runner_id = record.runner_id.as_deref().expect("checked runner_id");
        homeboy::core::runner::reap_run_workspace(runner_id, &record.path, None)?;
        return Ok(CleanupDispatch::Applied);
    }

    if record.runner_id.is_some() {
        return Ok(CleanupDispatch::Skipped(
            "remote-owned resource has no cleanup executor; leaving plan-only".to_string(),
        ));
    }

    match ResourceLifecycle::cleanup_path(root, record) {
        Ok(path) => {
            if args.apply {
                ResourceLifecycle::delete_path(&path)?;
                Ok(CleanupDispatch::Applied)
            } else {
                Ok(CleanupDispatch::Plan)
            }
        }
        Err(reason) => Ok(CleanupDispatch::Skipped(reason)),
    }
}

fn canonical_cleanup_root(root: &Path) -> Result<PathBuf> {
    let canonical = root.canonicalize().map_err(|error| {
        Error::validation_invalid_argument(
            "cleanup-root",
            format!("cleanup root must exist and be canonicalizable: {error}"),
            Some(root.display().to_string()),
            None,
        )
    })?;
    if !canonical.is_dir() {
        return Err(Error::validation_invalid_argument(
            "cleanup-root",
            "cleanup root must be a directory",
            Some(root.display().to_string()),
            None,
        ));
    }
    Ok(canonical)
}

fn cleanup_plan_item(
    record: &ResourceLifecycleRecord,
    operation: ResourceCleanupOperation,
) -> RunsResourcesCleanupPlanItem {
    RunsResourcesCleanupPlanItem {
        owner: record.owner.clone(),
        run_id: record.run_id.clone(),
        path: record.path.clone(),
        root_bound: record.root_bound.clone(),
        kind: record.kind.clone(),
        operation,
        cleanup_command: record.cleanup_command.clone(),
    }
}

fn cleanup_applied_item(
    record: &ResourceLifecycleRecord,
    operation: ResourceCleanupOperation,
) -> RunsResourcesCleanupAppliedItem {
    RunsResourcesCleanupAppliedItem {
        owner: record.owner.clone(),
        run_id: record.run_id.clone(),
        path: record.path.clone(),
        kind: record.kind.clone(),
        operation,
    }
}

fn cleanup_skip(
    record: &ResourceLifecycleRecord,
    reason: impl Into<String>,
) -> RunsResourcesCleanupSkippedItem {
    RunsResourcesCleanupSkippedItem {
        owner: record.owner.clone(),
        run_id: record.run_id.clone(),
        path: record.path.clone(),
        kind: record.kind.clone(),
        reason: reason.into(),
    }
}

struct LoadedResourceIndex {
    source: String,
    index: ResourceLifecycleIndex,
}

fn load_indexes(args: &RunsResourcesArgs) -> Result<Vec<LoadedResourceIndex>> {
    if args.sample {
        return Ok(vec![LoadedResourceIndex {
            source: "sample".to_string(),
            index: sample_resource_lifecycle_index(),
        }]);
    }

    if !args.file.is_empty() {
        return args.file.iter().map(|path| load_index_file(path)).collect();
    }

    let store = ObservationStore::open_initialized()?;
    load_observation_store_index(&store, args.run_id.as_deref())
}

fn load_observation_store_index(
    store: &ObservationStore,
    run_id: Option<&str>,
) -> Result<Vec<LoadedResourceIndex>> {
    let artifacts = if let Some(run_id) = run_id {
        store.list_artifacts(run_id)?
    } else {
        store
            .list_run_artifacts(
                RunListFilter {
                    limit: Some(1000),
                    ..Default::default()
                },
                None,
            )?
            .into_iter()
            .map(|run_artifact| run_artifact.artifact)
            .collect()
    };

    let mut index = resource_lifecycle_index_from_artifacts(&artifacts)?.unwrap_or_else(|| {
        ResourceLifecycleIndex {
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
            resources: Vec::new(),
        }
    });

    index
        .resources
        .extend(executor_evidence_resources(store, run_id)?);

    if index.resources.is_empty() {
        return Ok(Vec::new());
    }

    apply_terminal_workspace_transitions(store, &mut index)?;

    Ok(vec![LoadedResourceIndex {
        source: match run_id {
            Some(run_id) => format!("observation-store:{run_id}"),
            None => "observation-store".to_string(),
        },
        index,
    }])
}

fn executor_evidence_resources(
    store: &ObservationStore,
    run_id: Option<&str>,
) -> Result<Vec<ResourceLifecycleRecord>> {
    let root = homeboy::core::artifacts::root()?.join("agent-task/executor-evidence");
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut resources = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", root.display())))
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", root.display())))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(evidence_run_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if run_id.is_some_and(|run_id| run_id != evidence_run_id) {
            continue;
        }

        resources.push(executor_evidence_resource(
            store,
            &root,
            &path,
            evidence_run_id,
        )?);
    }

    Ok(resources)
}

fn executor_evidence_resource(
    store: &ObservationStore,
    root: &Path,
    path: &Path,
    run_id: String,
) -> Result<ResourceLifecycleRecord> {
    let status = match store.get_run(&run_id)? {
        Some(run) if is_terminal_run_status(&run.status) => {
            ResourceLifecycleResourceStatus::CleanupPending
        }
        Some(_) => ResourceLifecycleResourceStatus::Active,
        None => ResourceLifecycleResourceStatus::CleanupPending,
    };

    Ok(ResourceLifecycleRecord {
        owner: "agent-task.executor-evidence".to_string(),
        run_id: run_id.clone(),
        runner_id: None,
        path: path.display().to_string(),
        root_bound: Some(root.display().to_string()),
        kind: "executor_evidence_directory".to_string(),
        ttl: Some("P30D".to_string()),
        cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: ResourceCleanupIntent::DryRun,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {run_id} --cleanup-plan --cleanup-eligible"
        )),
        status,
    })
}

fn apply_terminal_workspace_transitions(
    store: &ObservationStore,
    index: &mut ResourceLifecycleIndex,
) -> Result<()> {
    let mut run_ids = index
        .resources
        .iter()
        .filter(|record| record.is_runner_workspace())
        .map(|record| record.run_id.clone())
        .collect::<Vec<_>>();
    run_ids.sort();
    run_ids.dedup();

    for run_id in run_ids {
        let Some(run) = store.get_run(&run_id)? else {
            continue;
        };
        if is_terminal_run_status(&run.status) {
            transition_preserved_runner_workspaces_to_cleanup_pending(
                index,
                &run_id,
                default_runner_workspace_ttl().as_str(),
            );
        }
    }

    Ok(())
}

fn default_runner_workspace_ttl() -> String {
    homeboy::core::defaults::load_config()
        .lab
        .runner_workspace_ttl
        .filter(|ttl| !ttl.trim().is_empty())
        .unwrap_or_else(|| "P7D".to_string())
}

fn is_terminal_run_status(status: &str) -> bool {
    RunStatus::from_label(status).is_some_and(RunStatus::is_terminal)
}

fn load_index_file(path: &Path) -> Result<LoadedResourceIndex> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let index: ResourceLifecycleIndex = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "file",
            format!(
                "{} is not a resource lifecycle index JSON file: {error}",
                path.display()
            ),
            Some(path.display().to_string()),
            None,
        )
    })?;
    index.validate()?;

    Ok(LoadedResourceIndex {
        source: path.display().to_string(),
        index,
    })
}

fn resource_matches_filters(record: &ResourceLifecycleRecord, args: &RunsResourcesArgs) -> bool {
    if args
        .run_id
        .as_deref()
        .is_some_and(|run_id| record.run_id != run_id)
    {
        return false;
    }

    if args
        .owner
        .as_deref()
        .is_some_and(|owner| record.owner != owner)
    {
        return false;
    }

    if args.actionable && !resource_lifecycle_record_is_actionable(record) {
        return false;
    }

    if args.cleanup_eligible && !resource_lifecycle_record_is_cleanup_eligible(record) {
        return false;
    }

    true
}

fn sample_resource_lifecycle_index() -> ResourceLifecycleIndex {
    ResourceLifecycleIndex {
        schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
        resources: vec![ResourceLifecycleRecord {
            owner: "homeboy-sample".to_string(),
            run_id: "sample-run-1".to_string(),
            runner_id: Some("sample-runner".to_string()),
            path: "/tmp/homeboy/sample-run-1/workspace".to_string(),
            root_bound: Some("/tmp/homeboy/sample-run-1".to_string()),
            kind: "workspace".to_string(),
            ttl: Some("2020-01-01T00:00:00Z".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Manifest,
            cleanup_intent: Default::default(),
            cleanup_command: Some(
                "homeboy runs resources --run-id sample-run-1 --cleanup-plan".to_string(),
            ),
            status: ResourceLifecycleResourceStatus::CleanupPending,
        }],
    }
}

#[cfg(test)]
mod tests {
    use homeboy::core::observation::{NewRunRecord, ObservationStore};
    use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
    use homeboy::core::resource_lifecycle_index::resource_lifecycle_record_is_cleanup_eligible_at;

    use super::*;

    fn temp_store(tempdir: &tempfile::TempDir) -> ObservationStore {
        ObservationStore::open_initialized_at(tempdir.path().join("homeboy.sqlite"))
            .expect("temp observation store")
    }

    fn record(status: ResourceLifecycleResourceStatus) -> ResourceLifecycleRecord {
        ResourceLifecycleRecord {
            owner: "owner-1".to_string(),
            run_id: "run-1".to_string(),
            runner_id: None,
            path: "/tmp/resource".to_string(),
            root_bound: None,
            kind: "workspace".to_string(),
            ttl: Some("2020-01-01T00:00:00Z".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Metadata,
            cleanup_intent: ResourceCleanupIntent::DryRun,
            cleanup_command: None,
            status,
        }
    }

    fn record_store_resource(
        store: &ObservationStore,
        run_id: &str,
        artifact_path: &Path,
        resource: ResourceLifecycleRecord,
    ) {
        store
            .record_artifact_with_metadata(
                run_id,
                "runner-workspace",
                artifact_path,
                serde_json::json!({
                    "resource_lifecycle": serde_json::to_value(resource)
                        .expect("resource lifecycle metadata"),
                }),
            )
            .expect("artifact with resource lifecycle metadata");
    }

    #[test]
    fn sample_resources_output_is_contract_valid() {
        let (RunsOutput::Resources(output), exit_code) = runs_resources(RunsResourcesArgs {
            sample: true,
            ..Default::default()
        })
        .expect("sample resources") else {
            panic!("expected resources output");
        };

        assert_eq!(exit_code, 0);
        assert_eq!(output.command, "runs.resources");
        assert_eq!(output.schema, RESOURCE_LIFECYCLE_INDEX_SCHEMA);
        assert_eq!(output.mode, ResourceCleanupMode::Inspect);
        assert_eq!(output.source_count, 1);
        assert_eq!(output.resource_count, 1);
        assert_eq!(output.actionable_count, 1);
        assert_eq!(output.cleanup_eligible_count, 1);
        assert!(output.cleanup.is_none());
    }

    #[test]
    fn filters_actionable_and_cleanup_eligible_resources() {
        let active = record(ResourceLifecycleResourceStatus::Active);
        let cleaned = ResourceLifecycleRecord {
            cleanup_policy: ResourceCleanupPolicy::Preserve,
            ..record(ResourceLifecycleResourceStatus::Cleaned)
        };
        let retained_apply = ResourceLifecycleRecord {
            cleanup_intent: ResourceCleanupIntent::Apply,
            cleanup_policy: ResourceCleanupPolicy::Manual,
            status: ResourceLifecycleResourceStatus::Retained,
            ..record(ResourceLifecycleResourceStatus::Retained)
        };

        assert!(resource_lifecycle_record_is_actionable(&active));
        assert!(!resource_lifecycle_record_is_actionable(&cleaned));
        assert!(resource_lifecycle_record_is_cleanup_eligible(&active));
        assert!(!resource_lifecycle_record_is_cleanup_eligible(&cleaned));
        assert!(resource_lifecycle_record_is_cleanup_eligible(
            &retained_apply
        ));
    }

    #[test]
    fn cleanup_plan_is_read_only_by_default() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource_path = tempdir.path().join("resource");
        std::fs::create_dir(&resource_path).expect("resource dir");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                cleanup_plan: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup plan");

        assert_eq!(cleanup.candidate_count, 1);
        assert_eq!(cleanup.planned_count, 1);
        assert_eq!(cleanup.planned[0].root_bound, None);
        assert_eq!(cleanup.applied_count, 0);
        assert!(resource_path.exists());
    }

    #[test]
    fn cleanup_plan_surfaces_root_bound_and_follow_up_command() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource_path = tempdir.path().join("resource");
        std::fs::create_dir(&resource_path).expect("resource dir");
        let cleanup_command = "homeboy runs resources --run-id run-1 --cleanup-plan".to_string();
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            root_bound: Some(tempdir.path().display().to_string()),
            cleanup_command: Some(cleanup_command.clone()),
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                cleanup_plan: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup plan");

        assert_eq!(cleanup.planned_count, 1);
        assert_eq!(
            cleanup.planned[0].root_bound,
            Some(tempdir.path().display().to_string())
        );
        assert_eq!(cleanup.planned[0].cleanup_command, Some(cleanup_command));
        assert!(resource_path.exists());
    }

    #[test]
    fn default_store_discovery_loads_resource_lifecycle_metadata() {
        crate::test_support::with_isolated_home(|_| {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let store = temp_store(&tempdir);
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");
            let artifact_path = tempdir.path().join("artifact.json");
            std::fs::write(&artifact_path, "{}").expect("artifact file");
            let resource = ResourceLifecycleRecord {
                run_id: run.id.clone(),
                ..record(ResourceLifecycleResourceStatus::CleanupPending)
            };
            record_store_resource(&store, &run.id, &artifact_path, resource.clone());

            let indexes = load_observation_store_index(&store, None).expect("store index");

            assert_eq!(indexes.len(), 1);
            assert_eq!(indexes[0].source, "observation-store");
            assert_eq!(indexes[0].index.resources, vec![resource]);
        });
    }

    #[test]
    fn default_store_discovery_includes_executor_evidence_directories() {
        crate::test_support::with_isolated_home(|_| {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let artifact_root = homeboy::core::artifacts::root().expect("artifact root");
            let store = temp_store(&tempdir);
            let run = store
                .start_run(NewRunRecord::builder("agent-task").build())
                .expect("run");
            let evidence_dir = artifact_root
                .join("agent-task")
                .join("executor-evidence")
                .join(&run.id);
            std::fs::create_dir_all(evidence_dir.join("task-1")).expect("executor evidence dir");
            std::fs::write(evidence_dir.join("task-1/executor-input.json"), "{}")
                .expect("evidence file");

            let indexes = load_observation_store_index(&store, Some(&run.id)).expect("store index");
            assert_eq!(indexes.len(), 1);
            assert_eq!(indexes[0].source, format!("observation-store:{}", run.id));
            assert_eq!(indexes[0].index.resources.len(), 1);

            let resource = &indexes[0].index.resources[0];
            assert_eq!(resource.owner, "agent-task.executor-evidence");
            assert_eq!(resource.run_id, run.id);
            assert_eq!(resource.path, evidence_dir.display().to_string());
            let expected_root_bound = artifact_root
                .join("agent-task/executor-evidence")
                .display()
                .to_string();
            assert_eq!(
                resource.root_bound.as_deref(),
                Some(expected_root_bound.as_str())
            );
            assert_eq!(resource.kind, "executor_evidence_directory");
            assert_eq!(resource.ttl.as_deref(), Some("P30D"));
            assert_eq!(
                resource.cleanup_policy,
                ResourceCleanupPolicy::DeleteAfterTtl
            );
            assert_eq!(
                resource.evidence_retention,
                ResourceEvidenceRetention::Metadata
            );
            assert_eq!(resource.cleanup_intent, ResourceCleanupIntent::DryRun);
            assert_eq!(resource.status, ResourceLifecycleResourceStatus::Active);
        });
    }

    #[test]
    fn terminal_executor_evidence_is_pending_but_ttl_gated_for_cleanup() {
        crate::test_support::with_isolated_home(|_| {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let artifact_root = homeboy::core::artifacts::root().expect("artifact root");
            let store = temp_store(&tempdir);
            let run = store
                .start_run(NewRunRecord::builder("agent-task").build())
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish run");
            let evidence_dir = artifact_root
                .join("agent-task")
                .join("executor-evidence")
                .join(&run.id);
            std::fs::create_dir_all(evidence_dir.join("task-1")).expect("executor evidence dir");
            std::fs::write(evidence_dir.join("task-1/executor-result.json"), "{}")
                .expect("evidence file");

            let indexes = load_observation_store_index(&store, Some(&run.id)).expect("store index");
            let resource = &indexes[0].index.resources[0];

            assert_eq!(
                resource.status,
                ResourceLifecycleResourceStatus::CleanupPending
            );
            assert!(!resource_lifecycle_record_is_cleanup_eligible(resource));
            assert!(resource_lifecycle_record_is_cleanup_eligible_at(
                resource,
                chrono::Utc::now() + chrono::Duration::days(31)
            ));
        });
    }

    #[test]
    fn cleanup_plan_outputs_executor_evidence_metadata_when_ttl_elapsed() {
        crate::test_support::with_isolated_home(|_| {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let artifact_root = homeboy::core::artifacts::root().expect("artifact root");
            let store = temp_store(&tempdir);
            let run_id = "orphaned-run";
            let evidence_root = artifact_root.join("agent-task").join("executor-evidence");
            let evidence_dir = evidence_root.join(run_id);
            std::fs::create_dir_all(evidence_dir.join("task-1")).expect("executor evidence dir");
            std::fs::write(evidence_dir.join("task-1/executor-input.json"), "{}")
                .expect("evidence file");

            let mut indexes =
                load_observation_store_index(&store, Some(run_id)).expect("store index");
            let mut resource = indexes.remove(0).index.resources.remove(0);
            resource.ttl = Some("0s".to_string());

            let cleanup = build_cleanup_output(
                &[resource],
                &RunsResourcesArgs {
                    cleanup_plan: true,
                    cleanup_root: Some(artifact_root.clone()),
                    cleanup_eligible: true,
                    ..Default::default()
                },
            )
            .expect("cleanup plan");

            assert_eq!(cleanup.candidate_count, 1);
            assert_eq!(cleanup.planned_count, 1);
            assert_eq!(cleanup.applied_count, 0);
            assert_eq!(cleanup.planned[0].owner, "agent-task.executor-evidence");
            assert_eq!(cleanup.planned[0].run_id, run_id);
            assert_eq!(cleanup.planned[0].path, evidence_dir.display().to_string());
            assert_eq!(
                cleanup.planned[0].root_bound,
                Some(evidence_root.display().to_string())
            );
            assert_eq!(cleanup.planned[0].kind, "executor_evidence_directory");
            assert_eq!(
                cleanup.planned[0].cleanup_command.as_deref(),
                Some("homeboy runs resources --run-id orphaned-run --cleanup-plan --cleanup-eligible")
            );
            assert!(evidence_dir.exists());
        });
    }

    #[test]
    fn store_discovered_cleanup_plan_remains_read_only() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let store = temp_store(&tempdir);
        let run = store
            .start_run(NewRunRecord::builder("runner-exec").build())
            .expect("run");
        let artifact_path = tempdir.path().join("artifact.json");
        std::fs::write(&artifact_path, "{}").expect("artifact file");
        let resource_path = tempdir.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            run_id: run.id.clone(),
            path: resource_path.display().to_string(),
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };
        record_store_resource(&store, &run.id, &artifact_path, resource);
        let indexes = load_observation_store_index(&store, Some(&run.id)).expect("store index");

        let cleanup = build_cleanup_output(
            &indexes[0].index.resources,
            &RunsResourcesArgs {
                cleanup_plan: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup plan");

        assert_eq!(cleanup.planned_count, 1);
        assert_eq!(cleanup.applied_count, 0);
        assert!(resource_path.exists());
    }

    #[test]
    fn store_discovered_apply_still_requires_apply_intent() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let store = temp_store(&tempdir);
        let run = store
            .start_run(NewRunRecord::builder("runner-exec").build())
            .expect("run");
        let artifact_path = tempdir.path().join("artifact.json");
        std::fs::write(&artifact_path, "{}").expect("artifact file");
        let resource_path = tempdir.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            run_id: run.id.clone(),
            path: resource_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::DryRun,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };
        record_store_resource(&store, &run.id, &artifact_path, resource);
        let indexes = load_observation_store_index(&store, Some(&run.id)).expect("store index");

        let cleanup = build_cleanup_output(
            &indexes[0].index.resources,
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup apply");

        assert_eq!(cleanup.applied_count, 0);
        assert_eq!(cleanup.skipped_count, 1);
        assert!(cleanup.skipped[0].reason.contains("explicit apply intent"));
        assert!(resource_path.exists());
    }

    #[test]
    fn apply_requires_explicit_apply_intent() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource_path = tempdir.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::DryRun,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup apply plan");

        assert_eq!(cleanup.applied_count, 0);
        assert_eq!(cleanup.skipped_count, 1);
        assert!(cleanup.skipped[0].reason.contains("explicit apply intent"));
        assert!(resource_path.exists());
    }

    #[test]
    fn apply_deletes_apply_intended_paths_under_root() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource_path = tempdir.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                limit: 1,
                ..Default::default()
            },
        )
        .expect("cleanup apply");

        assert_eq!(cleanup.applied_count, 1);
        assert!(!resource_path.exists());
    }

    #[test]
    fn apply_reaps_runner_workspace_through_runner_transport() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_root = tempfile::tempdir().expect("workspace root");
            let lab_root = workspace_root.path().join("_lab_workspaces");
            let resource_path = lab_root.join("repo");
            std::fs::create_dir_all(&resource_path).expect("runner workspace");
            std::fs::write(resource_path.join("generated.txt"), "generated")
                .expect("workspace file");
            homeboy::core::runner::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    workspace_root.path().display()
                ),
                false,
            )
            .expect("create local runner");
            let resource = ResourceLifecycleRecord {
                owner: "runner.workspace".to_string(),
                runner_id: Some("lab-local".to_string()),
                path: resource_path.display().to_string(),
                kind: "runner_workspace".to_string(),
                cleanup_intent: ResourceCleanupIntent::Apply,
                ..record(ResourceLifecycleResourceStatus::CleanupPending)
            };

            let cleanup = build_cleanup_output(
                &[resource],
                &RunsResourcesArgs {
                    apply: true,
                    cleanup_root: Some(workspace_root.path().to_path_buf()),
                    ..Default::default()
                },
            )
            .expect("cleanup apply");

            assert_eq!(cleanup.applied_count, 1);
            assert!(!resource_path.exists());
        });
    }

    #[test]
    fn apply_keeps_unsupported_remote_records_plan_only() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource = ResourceLifecycleRecord {
            owner: "homeboy-runs".to_string(),
            runner_id: Some("lab".to_string()),
            path: "/srv/homeboy/artifacts/run-1".to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup apply");

        assert_eq!(cleanup.applied_count, 0);
        assert_eq!(cleanup.skipped_count, 1);
        assert_eq!(
            cleanup.skipped[0].reason,
            "remote-owned resource has no cleanup executor; leaving plan-only"
        );
    }

    #[test]
    fn terminal_store_run_flips_preserved_runner_workspace_eligibility() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let store = temp_store(&tempdir);
        let run = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .expect("run");
        store
            .finish_run(&run.id, RunStatus::Pass, None)
            .expect("finish run");
        let artifact_path = tempdir.path().join("artifact.json");
        std::fs::write(&artifact_path, "{}").expect("artifact file");
        let resource = ResourceLifecycleRecord {
            owner: "runner.workspace".to_string(),
            run_id: run.id.clone(),
            runner_id: Some("lab".to_string()),
            path: "/srv/homeboy/_lab_workspaces/repo".to_string(),
            kind: "runner_workspace".to_string(),
            ttl: None,
            cleanup_policy: ResourceCleanupPolicy::Preserve,
            status: ResourceLifecycleResourceStatus::Active,
            ..record(ResourceLifecycleResourceStatus::Active)
        };
        store
            .record_artifact_with_metadata(
                &run.id,
                "lab-metadata",
                &artifact_path,
                serde_json::json!({ "workspace_resource_lifecycle": resource }),
            )
            .expect("artifact with resource lifecycle metadata");

        let indexes = load_observation_store_index(&store, Some(&run.id)).expect("store index");

        let transitioned = &indexes[0].index.resources[0];
        assert_eq!(
            transitioned.cleanup_policy,
            ResourceCleanupPolicy::DeleteAfterTtl
        );
        assert_eq!(transitioned.ttl.as_deref(), Some("P7D"));
        assert_eq!(transitioned.cleanup_intent, ResourceCleanupIntent::Apply);
        assert_eq!(
            transitioned.status,
            ResourceLifecycleResourceStatus::CleanupPending
        );
        assert!(resource_lifecycle_record_is_cleanup_eligible(transitioned));
    }

    #[test]
    fn apply_skips_paths_outside_root() {
        let root = tempfile::tempdir().expect("root tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let resource_path = outside.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[resource],
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(root.path().to_path_buf()),
                ..Default::default()
            },
        )
        .expect("cleanup apply");

        assert_eq!(cleanup.applied_count, 0);
        assert_eq!(cleanup.skipped_count, 1);
        assert_eq!(
            cleanup.skipped[0].reason,
            "resource path is outside cleanup root"
        );
        assert!(resource_path.exists());
    }

    #[test]
    fn cleanup_apply_is_bounded_by_limit() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let first_path = tempdir.path().join("first");
        let second_path = tempdir.path().join("second");
        std::fs::write(&first_path, "generated").expect("first file");
        std::fs::write(&second_path, "generated").expect("second file");
        let first = ResourceLifecycleRecord {
            path: first_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };
        let second = ResourceLifecycleRecord {
            path: second_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record(ResourceLifecycleResourceStatus::CleanupPending)
        };

        let cleanup = build_cleanup_output(
            &[first, second],
            &RunsResourcesArgs {
                apply: true,
                cleanup_root: Some(tempdir.path().to_path_buf()),
                limit: 1,
                ..Default::default()
            },
        )
        .expect("cleanup apply");

        assert_eq!(cleanup.applied_count, 1);
        assert_eq!(cleanup.deferred_count, 1);
        assert!(!first_path.exists());
        assert!(second_path.exists());
    }
}
