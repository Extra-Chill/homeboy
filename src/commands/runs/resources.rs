use std::path::{Path, PathBuf};

use clap::Args;
use homeboy::core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleIndex,
    ResourceLifecycleRecord, ResourceLifecycleResourceStatus, RESOURCE_LIFECYCLE_INDEX_SCHEMA,
};
use homeboy::core::{Error, Result};
use serde::Serialize;

use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Default)]
pub struct RunsResourcesArgs {
    /// Resource lifecycle index JSON file. Repeatable until producers publish a canonical store.
    #[arg(long = "file", value_name = "PATH")]
    pub file: Vec<PathBuf>,

    /// Emit a contract-valid sample index instead of reading files.
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
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesOutput {
    pub command: &'static str,
    pub schema: &'static str,
    pub source_count: usize,
    pub resource_count: usize,
    pub actionable_count: usize,
    pub cleanup_eligible_count: usize,
    pub sources: Vec<RunsResourcesSourceOutput>,
    pub resources: Vec<ResourceLifecycleRecord>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RunsResourcesSourceOutput {
    pub source: String,
    pub resource_count: usize,
}

pub fn runs_resources(args: RunsResourcesArgs) -> CmdResult<RunsOutput> {
    if !args.sample && args.file.is_empty() {
        return Err(Error::validation_invalid_argument(
            "file",
            "provide at least one --file path, or pass --sample for a contract-valid sample resource index",
            None,
            Some(vec![
                "Until resource producers publish a canonical store, this command reads explicit resource lifecycle index JSON files.".to_string(),
                "Run `homeboy runs resources --sample` to inspect the expected output contract.".to_string(),
            ]),
        ));
    }

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
        .filter(|record| resource_is_actionable(record))
        .count();
    let cleanup_eligible_count = all_resources
        .iter()
        .filter(|record| resource_is_cleanup_eligible(record))
        .count();

    let resources = all_resources
        .into_iter()
        .filter(|record| resource_matches_filters(record, &args))
        .collect::<Vec<_>>();

    Ok((
        RunsOutput::Resources(RunsResourcesOutput {
            command: "runs.resources",
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA,
            source_count,
            resource_count: resources.len(),
            actionable_count,
            cleanup_eligible_count,
            sources,
            resources,
        }),
        0,
    ))
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

    args.file.iter().map(|path| load_index_file(path)).collect()
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

    if args.actionable && !resource_is_actionable(record) {
        return false;
    }

    if args.cleanup_eligible && !resource_is_cleanup_eligible(record) {
        return false;
    }

    true
}

fn resource_is_actionable(record: &ResourceLifecycleRecord) -> bool {
    matches!(
        record.status,
        ResourceLifecycleResourceStatus::Active
            | ResourceLifecycleResourceStatus::CleanupPending
            | ResourceLifecycleResourceStatus::Missing
            | ResourceLifecycleResourceStatus::Failed
    )
}

fn resource_is_cleanup_eligible(record: &ResourceLifecycleRecord) -> bool {
    if matches!(record.status, ResourceLifecycleResourceStatus::Cleaned) {
        return false;
    }

    if record.cleanup_intent.is_apply() {
        return true;
    }

    matches!(
        record.status,
        ResourceLifecycleResourceStatus::CleanupPending
    ) || matches!(
        record.cleanup_policy,
        ResourceCleanupPolicy::DeleteAfterTtl
            | ResourceCleanupPolicy::DeleteOnSuccess
            | ResourceCleanupPolicy::DeleteOnTerminal
    )
}

fn sample_resource_lifecycle_index() -> ResourceLifecycleIndex {
    ResourceLifecycleIndex {
        schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
        resources: vec![ResourceLifecycleRecord {
            owner: "homeboy-sample".to_string(),
            run_id: "sample-run-1".to_string(),
            runner_id: Some("sample-runner".to_string()),
            path: "/tmp/homeboy/sample-run-1/workspace".to_string(),
            kind: "workspace".to_string(),
            ttl: Some("P7D".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Manifest,
            cleanup_intent: Default::default(),
            status: ResourceLifecycleResourceStatus::CleanupPending,
        }],
    }
}

#[cfg(test)]
mod tests {
    use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;

    use super::*;

    fn record(status: ResourceLifecycleResourceStatus) -> ResourceLifecycleRecord {
        ResourceLifecycleRecord {
            owner: "owner-1".to_string(),
            run_id: "run-1".to_string(),
            runner_id: None,
            path: "/tmp/resource".to_string(),
            kind: "workspace".to_string(),
            ttl: Some("P1D".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Metadata,
            cleanup_intent: ResourceCleanupIntent::DryRun,
            status,
        }
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
        assert_eq!(output.source_count, 1);
        assert_eq!(output.resource_count, 1);
        assert_eq!(output.actionable_count, 1);
        assert_eq!(output.cleanup_eligible_count, 1);
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

        assert!(resource_is_actionable(&active));
        assert!(!resource_is_actionable(&cleaned));
        assert!(resource_is_cleanup_eligible(&active));
        assert!(!resource_is_cleanup_eligible(&cleaned));
        assert!(resource_is_cleanup_eligible(&retained_apply));
    }
}
