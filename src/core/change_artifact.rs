//! Shared vocabulary for code-change artifacts and apply results.
//!
//! This module intentionally stays small: producers can describe a patch or
//! file delta with provenance, while apply surfaces can report a common result
//! envelope without adopting a full runner/Lab/refactor rewrite at once.

use serde::{Deserialize, Serialize};

use crate::core::source_snapshot::SourceSnapshot;

pub const CHANGE_ARTIFACT_SCHEMA: &str = "homeboy/change-artifact/v1";
pub const CHANGE_APPLY_RESULT_SCHEMA: &str = "homeboy/change-apply-result/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeArtifact {
    #[serde(default = "default_change_artifact_schema")]
    pub schema: String,
    pub source_snapshot: SourceSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<ChangePatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<ChangeDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ChangeArtifactProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ChangeArtifactDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangePatch {
    #[serde(default = "default_patch_format")]
    pub format: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeDelta {
    pub files: Vec<ChangeDeltaFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeDeltaFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeArtifactProvenance {
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeArtifactDigest {
    #[serde(default = "default_digest_algorithm")]
    pub algorithm: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeApplyResult {
    #[serde(default = "default_change_apply_result_schema")]
    pub schema: String,
    pub apply_status: ChangeApplyStatus,
    pub force: bool,
    pub expected_snapshot_hash: String,
    pub current_snapshot_hash: String,
    pub modified_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ChangeArtifactSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeApplyStatus {
    Applied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeArtifactSummary {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ChangeArtifactProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ChangeArtifactDigest>,
}

impl ChangeArtifact {
    pub fn summary(&self) -> ChangeArtifactSummary {
        ChangeArtifactSummary {
            schema: self.schema.clone(),
            provenance: self.provenance.clone(),
            digest: self.digest.clone(),
        }
    }
}

impl ChangeApplyResult {
    pub fn applied(
        force: bool,
        expected_snapshot_hash: String,
        current_snapshot_hash: String,
        modified_files: Vec<String>,
        artifact: Option<ChangeArtifactSummary>,
    ) -> Self {
        Self {
            schema: CHANGE_APPLY_RESULT_SCHEMA.to_string(),
            apply_status: ChangeApplyStatus::Applied,
            force,
            expected_snapshot_hash,
            current_snapshot_hash,
            modified_files,
            artifact,
        }
    }
}

fn default_change_artifact_schema() -> String {
    CHANGE_ARTIFACT_SCHEMA.to_string()
}

fn default_change_apply_result_schema() -> String {
    CHANGE_APPLY_RESULT_SCHEMA.to_string()
}

fn default_patch_format() -> String {
    "unified_diff".to_string()
}

fn default_digest_algorithm() -> String {
    "sha256".to_string()
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_change_artifact_with_snapshot_provenance_and_digest() {
        let artifact = ChangeArtifact {
            schema: CHANGE_ARTIFACT_SCHEMA.to_string(),
            source_snapshot: source_snapshot(),
            patch: Some(ChangePatch {
                format: "unified_diff".to_string(),
                content: "diff --git a/a b/a\n".to_string(),
            }),
            delta: None,
            provenance: Some(ChangeArtifactProvenance {
                producer: "runner.capture_patch".to_string(),
                run_id: Some("run-1".to_string()),
                artifact_id: Some("patch.diff".to_string()),
                command: Some(vec!["homeboy".to_string(), "lab".to_string()]),
            }),
            digest: Some(ChangeArtifactDigest {
                algorithm: "sha256".to_string(),
                value: "abc123".to_string(),
            }),
        };

        let json = serde_json::to_value(&artifact).expect("serialize artifact");

        assert_eq!(json["schema"], CHANGE_ARTIFACT_SCHEMA);
        assert_eq!(json["source_snapshot"]["snapshot_hash"], "sha256:snapshot");
        assert_eq!(json["provenance"]["producer"], "runner.capture_patch");
        assert_eq!(json["digest"]["algorithm"], "sha256");
        assert_eq!(json["digest"]["value"], "abc123");
    }

    #[test]
    fn serializes_apply_result_shape_with_artifact_summary() {
        let result = ChangeApplyResult::applied(
            true,
            "sha256:before".to_string(),
            "sha256:after".to_string(),
            vec!["src/lib.rs".to_string()],
            Some(ChangeArtifactSummary {
                schema: CHANGE_ARTIFACT_SCHEMA.to_string(),
                provenance: Some(ChangeArtifactProvenance {
                    producer: "refactor.transform".to_string(),
                    run_id: None,
                    artifact_id: None,
                    command: None,
                }),
                digest: Some(ChangeArtifactDigest {
                    algorithm: "sha256".to_string(),
                    value: "def456".to_string(),
                }),
            }),
        );

        let json = serde_json::to_value(&result).expect("serialize apply result");

        assert_eq!(json["schema"], CHANGE_APPLY_RESULT_SCHEMA);
        assert_eq!(json["apply_status"], "applied");
        assert_eq!(json["force"], true);
        assert_eq!(json["modified_files"][0], "src/lib.rs");
        assert_eq!(json["artifact"]["schema"], CHANGE_ARTIFACT_SCHEMA);
        assert_eq!(
            json["artifact"]["provenance"]["producer"],
            "refactor.transform"
        );
        assert_eq!(json["artifact"]["digest"]["value"], "def456");
    }

    fn source_snapshot() -> SourceSnapshot {
        SourceSnapshot {
            runner_id: "lab-local".to_string(),
            local_path: Some("/tmp/homeboy".to_string()),
            remote_path: Some("/srv/homeboy".to_string()),
            workspace_root: Some("/tmp/homeboy".to_string()),
            git_branch: Some("main".to_string()),
            sync_mode: "snapshot".to_string(),
            git_sha: Some("abc".to_string()),
            dirty: false,
            snapshot_hash: "sha256:snapshot".to_string(),
            synced_at: "2026-05-31T00:00:00Z".to_string(),
            sync_excludes: Vec::new(),
        }
    }
}
