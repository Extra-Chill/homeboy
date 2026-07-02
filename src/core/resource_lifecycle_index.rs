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
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub evidence_retention: ResourceEvidenceRetention,
    #[serde(default)]
    pub cleanup_intent: ResourceCleanupIntent,
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
    pub kind: String,
    pub status: ResourceLifecycleResourceStatus,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub cleanup_intent: ResourceCleanupIntent,
    pub actionable: bool,
    pub cleanup_eligible: bool,
}

impl ResourceLifecycleInspection {
    pub fn from_record(record: &ResourceLifecycleRecord) -> Self {
        Self {
            owner: record.owner.clone(),
            run_id: record.run_id.clone(),
            path: record.path.clone(),
            kind: record.kind.clone(),
            status: record.status,
            cleanup_policy: record.cleanup_policy,
            cleanup_intent: record.cleanup_intent,
            actionable: resource_lifecycle_record_is_actionable(record),
            cleanup_eligible: resource_lifecycle_record_is_cleanup_eligible(record),
        }
    }
}

impl ResourceLifecycleRecord {
    pub fn validate(&self, index: usize) -> Result<()> {
        validate_resource_lifecycle_record(index, self)
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
        let Some(value) = artifact.metadata_json.get("resource_lifecycle") else {
            continue;
        };
        let record: ResourceLifecycleRecord =
            serde_json::from_value(value.clone()).map_err(|err| {
                Error::internal_json(
                    err.to_string(),
                    Some(format!(
                        "parse resource lifecycle metadata for artifact {}",
                        artifact.id
                    )),
                )
            })?;
        resources.push(record);
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
            kind: "workspace".to_string(),
            ttl: Some("P7D".to_string()),
            cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
            evidence_retention: ResourceEvidenceRetention::Manifest,
            cleanup_intent: ResourceCleanupIntent::DryRun,
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
