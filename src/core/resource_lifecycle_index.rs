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
}
