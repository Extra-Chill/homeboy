use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::core::observation::ArtifactRecord;
use crate::core::resource_cleanup_intent::ResourceCleanupIntent;
use crate::core::{Error, Result};

pub const RESOURCE_LIFECYCLE_INDEX_SCHEMA: &str = "homeboy/resource-lifecycle-index/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceLifecycleIndex {
    #[serde(default = "resource_lifecycle_index_schema")]
    pub schema: String,
    #[serde(default)]
    pub resources: Vec<ResourceLifecycleRecord>,
}

impl ResourceLifecycleIndex {
    pub fn validate(&self) -> Result<()> {
        validate_resource_lifecycle_index(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceLifecycleRecord {
    pub owner: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_bound: Option<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub evidence_retention: ResourceEvidenceRetention,
    #[serde(default)]
    pub cleanup_intent: ResourceCleanupIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
    pub status: ResourceLifecycleResourceStatus,
}

pub struct ResourceLifecycle;

impl ResourceLifecycle {
    pub fn inspect(record: &ResourceLifecycleRecord) -> ResourceLifecycleInspection {
        ResourceLifecycleInspection::from_record(record)
    }

    pub fn cleanup_path(
        root: &Path,
        record: &ResourceLifecycleRecord,
    ) -> std::result::Result<PathBuf, String> {
        resource_lifecycle_cleanup_path(root, record)
    }

    pub fn delete_path(path: &Path) -> Result<()> {
        delete_resource_lifecycle_path(path)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceLifecycleInspection {
    pub owner: String,
    pub run_id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_bound: Option<String>,
    pub kind: String,
    pub status: ResourceLifecycleResourceStatus,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub cleanup_intent: ResourceCleanupIntent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
    pub actionable: bool,
    pub cleanup_eligible: bool,
}

impl ResourceLifecycleInspection {
    pub fn from_record(record: &ResourceLifecycleRecord) -> Self {
        Self {
            owner: record.owner.clone(),
            run_id: record.run_id.clone(),
            path: record.path.clone(),
            root_bound: record.root_bound.clone(),
            kind: record.kind.clone(),
            status: record.status,
            cleanup_policy: record.cleanup_policy,
            cleanup_intent: record.cleanup_intent,
            cleanup_command: record.cleanup_command.clone(),
            actionable: resource_lifecycle_record_is_actionable(record),
            cleanup_eligible: resource_lifecycle_record_is_cleanup_eligible(record),
        }
    }
}

impl ResourceLifecycleRecord {
    pub fn validate(&self, index: usize) -> Result<()> {
        validate_resource_lifecycle_record(index, self)
    }

    pub fn is_runner_workspace(&self) -> bool {
        self.owner == "runner.workspace" && self.kind == "runner_workspace"
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCleanupPolicy {
    Preserve,
    Manual,
    DeleteAfterTtl,
    DeleteOnSuccess,
    DeleteOnTerminal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEvidenceRetention {
    None,
    Metadata,
    Manifest,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLifecycleResourceStatus {
    Declared,
    Active,
    Retained,
    CleanupPending,
    Cleaned,
    Missing,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLifecycleCleanupOperation {
    Delete,
}

impl Default for ResourceLifecycleCleanupOperation {
    fn default() -> Self {
        Self::Delete
    }
}

impl std::fmt::Display for ResourceLifecycleCleanupOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Delete => formatter.write_str("delete"),
        }
    }
}

pub fn resource_lifecycle_record_is_actionable(record: &ResourceLifecycleRecord) -> bool {
    matches!(
        record.status,
        ResourceLifecycleResourceStatus::Active
            | ResourceLifecycleResourceStatus::CleanupPending
            | ResourceLifecycleResourceStatus::Missing
            | ResourceLifecycleResourceStatus::Failed
    )
}

pub fn resource_lifecycle_record_is_cleanup_eligible(record: &ResourceLifecycleRecord) -> bool {
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

pub fn resource_lifecycle_cleanup_path(
    root: &Path,
    record: &ResourceLifecycleRecord,
) -> std::result::Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "cleanup root does not exist".to_string())?;
    let root_bound = record
        .root_bound
        .as_deref()
        .map(|path| {
            PathBuf::from(path)
                .canonicalize()
                .map_err(|_| "declared root bound does not exist".to_string())
        })
        .transpose()?;
    let path = PathBuf::from(&record.path);
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| "resource path does not exist".to_string())?;
    let cleanup_path = if metadata.file_type().is_symlink() {
        path.clone()
    } else {
        path.canonicalize()
            .map_err(|_| "resource path does not exist".to_string())?
    };
    let containment_path = if metadata.file_type().is_symlink() {
        let parent = path
            .parent()
            .ok_or_else(|| "resource path has no parent directory".to_string())?
            .canonicalize()
            .map_err(|_| "resource parent path does not exist".to_string())?;
        let file_name = path
            .file_name()
            .ok_or_else(|| "resource path has no file name".to_string())?;
        parent.join(file_name)
    } else {
        cleanup_path.clone()
    };

    if containment_path == root {
        return Err("resource path is cleanup root".to_string());
    }

    if !containment_path.starts_with(&root) {
        return Err("resource path is outside cleanup root".to_string());
    }

    if let Some(root_bound) = &root_bound {
        if containment_path == *root_bound {
            return Err("resource path is declared root bound".to_string());
        }
        if !containment_path.starts_with(root_bound) {
            return Err("resource path is outside declared root bound".to_string());
        }
    }

    Ok(cleanup_path)
}

pub fn delete_resource_lifecycle_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;

    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("delete {}", path.display())),
            )
        })
    } else {
        fs::remove_file(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("delete {}", path.display())),
            )
        })
    }
}

pub fn validate_resource_lifecycle_index(index: &ResourceLifecycleIndex) -> Result<()> {
    if index.schema != RESOURCE_LIFECYCLE_INDEX_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "expected {RESOURCE_LIFECYCLE_INDEX_SCHEMA}, received {}",
                index.schema
            ),
            Some(index.schema.clone()),
            None,
        ));
    }

    for (resource_index, resource) in index.resources.iter().enumerate() {
        validate_resource_lifecycle_record(resource_index, resource)?;
    }

    Ok(())
}

pub fn resource_lifecycle_index_from_artifacts(
    artifacts: &[ArtifactRecord],
) -> Result<Option<ResourceLifecycleIndex>> {
    let mut resources = Vec::new();
    for artifact in artifacts {
        if let Some(value) = artifact.metadata_json.get("resource_lifecycle_index") {
            let index: ResourceLifecycleIndex =
                serde_json::from_value(value.clone()).map_err(|err| {
                    Error::internal_json(
                        err.to_string(),
                        Some(format!(
                            "parse resource lifecycle index metadata for artifact {}",
                            artifact.id
                        )),
                    )
                })?;
            index.validate()?;
            resources.extend(index.resources);
            continue;
        }

        for key in ["resource_lifecycle", "workspace_resource_lifecycle"] {
            let Some(value) = artifact.metadata_json.get(key) else {
                continue;
            };
            let record: ResourceLifecycleRecord =
                serde_json::from_value(value.clone()).map_err(|err| {
                    Error::internal_json(
                        err.to_string(),
                        Some(format!("parse {key} metadata for artifact {}", artifact.id)),
                    )
                })?;
            resources.push(record);
        }
    }
    if resources.is_empty() {
        return Ok(None);
    }

    let index = ResourceLifecycleIndex {
        schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
        resources,
    };
    index.validate()?;
    Ok(Some(index))
}

pub fn transition_preserved_runner_workspaces_to_cleanup_pending(
    index: &mut ResourceLifecycleIndex,
    run_id: &str,
) {
    for record in &mut index.resources {
        if record.run_id == run_id
            && record.is_runner_workspace()
            && record.runner_id.is_some()
            && matches!(record.cleanup_policy, ResourceCleanupPolicy::Preserve)
            && matches!(record.status, ResourceLifecycleResourceStatus::Active)
        {
            record.cleanup_policy = ResourceCleanupPolicy::DeleteAfterTtl;
            record.ttl.get_or_insert_with(|| "P7D".to_string());
            record.status = ResourceLifecycleResourceStatus::CleanupPending;
        }
    }
}

pub fn validate_resource_lifecycle_record(
    index: usize,
    record: &ResourceLifecycleRecord,
) -> Result<()> {
    let prefix = format!("resources[{index}]");
    validate_required_field(&format!("{prefix}.owner"), &record.owner)?;
    validate_required_field(&format!("{prefix}.run_id"), &record.run_id)?;
    validate_required_field(&format!("{prefix}.path"), &record.path)?;
    validate_required_field(&format!("{prefix}.kind"), &record.kind)?;

    if let Some(runner_id) = &record.runner_id {
        validate_required_field(&format!("{prefix}.runner_id"), runner_id)?;
    }

    if let Some(root_bound) = &record.root_bound {
        validate_required_field(&format!("{prefix}.root_bound"), root_bound)?;
    }

    if let Some(cleanup_command) = &record.cleanup_command {
        validate_required_field(&format!("{prefix}.cleanup_command"), cleanup_command)?;
    }

    if let Some(ttl) = &record.ttl {
        validate_required_field(&format!("{prefix}.ttl"), ttl)?;
    } else if matches!(record.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl) {
        return Err(Error::validation_invalid_argument(
            format!("{prefix}.ttl"),
            "delete_after_ttl cleanup policy requires ttl",
            None,
            None,
        ));
    }

    Ok(())
}

fn validate_required_field(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "must not be blank",
            None,
            None,
        ));
    }

    Ok(())
}

fn resource_lifecycle_index_schema() -> String {
    RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ErrorCode;

    fn record() -> ResourceLifecycleRecord {
        ResourceLifecycleRecord {
            owner: "homeboy-test".to_string(),
            run_id: "run-1".to_string(),
            runner_id: Some("runner-1".to_string()),
            path: "/tmp/homeboy/run-1/workspace".to_string(),
            root_bound: None,
            kind: "workspace".to_string(),
            ttl: Some("P7D".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Manifest,
            cleanup_intent: ResourceCleanupIntent::DryRun,
            cleanup_command: Some(
                "homeboy runs resources --run-id run-1 --cleanup-plan".to_string(),
            ),
            status: ResourceLifecycleResourceStatus::Active,
        }
    }

    fn index() -> ResourceLifecycleIndex {
        ResourceLifecycleIndex {
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
            resources: vec![record()],
        }
    }

    #[test]
    fn validates_resource_lifecycle_index() {
        index().validate().unwrap();
    }

    #[test]
    fn validates_root_bound_and_follow_up_cleanup_command() {
        let mut index = index();
        index.resources[0].root_bound = Some("/tmp/homeboy/run-1".to_string());
        index.resources[0].cleanup_command =
            Some("homeboy runs resources --run-id run-1 --cleanup-plan".to_string());

        index.validate().unwrap();
    }

    #[test]
    fn extracts_resource_lifecycle_index_artifact_metadata() {
        let source = ResourceLifecycleIndex {
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
            resources: vec![record()],
        };
        let artifact = ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "resource_lifecycle_index".to_string(),
            artifact_type: "file".to_string(),
            path: "/tmp/index.json".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::json!({
                "resource_lifecycle_index": source,
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let index = resource_lifecycle_index_from_artifacts(&[artifact])
            .expect("parse index metadata")
            .expect("index present");

        assert_eq!(index.resources.len(), 1);
        assert_eq!(index.resources[0].kind, "workspace");
    }

    #[test]
    fn extracts_workspace_resource_lifecycle_artifact_metadata() {
        let mut source = record();
        source.owner = "runner.workspace".to_string();
        source.kind = "runner_workspace".to_string();
        let artifact = ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "lab-metadata".to_string(),
            artifact_type: "metadata".to_string(),
            path: "/tmp/index.json".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::json!({
                "workspace_resource_lifecycle": source,
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let index = resource_lifecycle_index_from_artifacts(&[artifact])
            .expect("parse index metadata")
            .expect("index present");

        assert_eq!(index.resources.len(), 1);
        assert_eq!(index.resources[0].owner, "runner.workspace");
    }

    #[test]
    fn terminal_transition_flips_preserved_runner_workspace_eligibility() {
        let mut index = ResourceLifecycleIndex {
            schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
            resources: vec![ResourceLifecycleRecord {
                owner: "runner.workspace".to_string(),
                run_id: "run-1".to_string(),
                runner_id: Some("lab".to_string()),
                path: "/srv/homeboy/_lab_workspaces/repo".to_string(),
                root_bound: None,
                kind: "runner_workspace".to_string(),
                ttl: None,
                cleanup_policy: ResourceCleanupPolicy::Preserve,
                evidence_retention: ResourceEvidenceRetention::Metadata,
                cleanup_intent: ResourceCleanupIntent::DryRun,
                cleanup_command: None,
                status: ResourceLifecycleResourceStatus::Active,
            }],
        };

        assert!(!resource_lifecycle_record_is_cleanup_eligible(
            &index.resources[0]
        ));

        transition_preserved_runner_workspaces_to_cleanup_pending(&mut index, "run-1");

        assert_eq!(
            index.resources[0].cleanup_policy,
            ResourceCleanupPolicy::DeleteAfterTtl
        );
        assert_eq!(index.resources[0].ttl.as_deref(), Some("P7D"));
        assert_eq!(
            index.resources[0].status,
            ResourceLifecycleResourceStatus::CleanupPending
        );
        assert!(resource_lifecycle_record_is_cleanup_eligible(
            &index.resources[0]
        ));
    }

    #[test]
    fn rejects_wrong_schema() {
        let mut index = index();
        index.schema = "homeboy/resource-lifecycle-index/v0".to_string();

        let error = index.validate().unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "schema");
    }

    #[test]
    fn rejects_blank_required_resource_fields() {
        let mut index = index();
        index.resources[0].path = "  ".to_string();

        let error = index.validate().unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "resources[0].path");
    }

    #[test]
    fn rejects_blank_root_bound_and_cleanup_command() {
        let mut contract = index();
        contract.resources[0].root_bound = Some("  ".to_string());

        let error = contract.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "resources[0].root_bound");

        let mut contract = index();
        contract.resources[0].cleanup_command = Some("  ".to_string());

        let error = contract.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "resources[0].cleanup_command");
    }

    #[test]
    fn delete_after_ttl_requires_ttl() {
        let mut index = index();
        index.resources[0].ttl = None;

        let error = index.validate().unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "resources[0].ttl");
    }

    #[test]
    fn serializes_stable_snake_case_policy_values() {
        assert_eq!(
            serde_json::to_string(&ResourceCleanupPolicy::DeleteAfterTtl).unwrap(),
            "\"delete_after_ttl\""
        );
        assert_eq!(
            serde_json::to_string(&ResourceLifecycleResourceStatus::CleanupPending).unwrap(),
            "\"cleanup_pending\""
        );
    }

    #[test]
    fn inspection_reports_actionable_cleanup_eligible_records() {
        let inspection = ResourceLifecycle::inspect(&record());

        assert_eq!(inspection.owner, "homeboy-test");
        assert!(inspection.actionable);
        assert!(inspection.cleanup_eligible);
    }

    #[test]
    fn cleanup_path_rejects_cleanup_root_itself() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let resource = ResourceLifecycleRecord {
            path: tempdir.path().display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record()
        };

        let error = ResourceLifecycle::cleanup_path(tempdir.path(), &resource)
            .expect_err("cleanup root must not be a candidate");

        assert_eq!(error, "resource path is cleanup root");
    }

    #[test]
    fn cleanup_path_rejects_paths_outside_root() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let resource_path = outside.path().join("resource");
        std::fs::write(&resource_path, "generated").expect("write resource");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record()
        };

        let error = ResourceLifecycle::cleanup_path(root.path(), &resource)
            .expect_err("outside path must be rejected");

        assert_eq!(error, "resource path is outside cleanup root");
    }

    #[test]
    fn cleanup_path_enforces_declared_root_bound() {
        let cleanup_root = tempfile::tempdir().expect("cleanup root");
        let bound = cleanup_root.path().join("owned");
        let sibling = cleanup_root.path().join("other");
        std::fs::create_dir(&bound).expect("bound dir");
        std::fs::create_dir(&sibling).expect("sibling dir");
        let resource_path = sibling.join("resource");
        std::fs::write(&resource_path, "generated").expect("resource file");
        let resource = ResourceLifecycleRecord {
            path: resource_path.display().to_string(),
            root_bound: Some(bound.display().to_string()),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record()
        };

        let error = ResourceLifecycle::cleanup_path(cleanup_root.path(), &resource)
            .expect_err("outside root bound must be rejected");

        assert_eq!(error, "resource path is outside declared root bound");
        assert!(resource_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_path_for_symlink_deletes_link_not_target() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("target");
        std::fs::write(&target, "target").expect("write target");
        let link = root.path().join("resource-link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let resource = ResourceLifecycleRecord {
            path: link.display().to_string(),
            cleanup_intent: ResourceCleanupIntent::Apply,
            ..record()
        };

        let cleanup_path = ResourceLifecycle::cleanup_path(root.path(), &resource)
            .expect("symlink under root is cleanup eligible");
        ResourceLifecycle::delete_path(&cleanup_path).expect("delete link");

        assert!(!link.exists());
        assert!(target.exists());
    }
}
