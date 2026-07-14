use std::path::Path;

use crate::core::observation::{LAB_OFFLOAD_METADATA_ENV, SOURCE_SNAPSHOT_METADATA_ENV};
use crate::core::source_snapshot::SourceSnapshot;

use super::workspace_content_hash;

const LAB_SOURCE_SNAPSHOT_SYNC_MODE: &str = "lab_offload";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedLabWorkspaceProvenance {
    pub source_revision: String,
    pub materialization_mode: String,
    pub runner_id: String,
    pub workspace_identity: String,
}

/// Verifies a Lab-materialized workspace against the controller-produced
/// content digest. Environment values transport the contract; the remote bytes
/// are the authority for the content match.
pub(crate) fn verify_lab_workspace_from_env(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    let snapshot: SourceSnapshot = env_json(SOURCE_SNAPSHOT_METADATA_ENV)
        .ok_or_else(|| "is missing source snapshot transport metadata".to_string())?;
    let lab: serde_json::Value = env_json(LAB_OFFLOAD_METADATA_ENV)
        .ok_or_else(|| "is missing Lab dispatch transport metadata".to_string())?;
    verify_lab_workspace(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )
}

pub(crate) fn verify_lab_workspace(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
    snapshot: SourceSnapshot,
    lab: serde_json::Value,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    snapshot
        .local_path
        .as_deref()
        .ok_or("is missing controller source path")?;
    let recorded_remote_path = snapshot
        .remote_path
        .as_deref()
        .ok_or("is missing remote path")?;
    let source_revision = snapshot
        .git_sha
        .as_deref()
        .ok_or("is missing source revision")?;
    let workspace_identity = snapshot
        .workspace_snapshot_identity
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("is missing workspace identity")?;
    let runner_id = lab
        .get("runner_id")
        .and_then(|value| value.as_str())
        .ok_or("is missing runner identity")?;
    let lab_remote_path = lab
        .get("remote_workspace")
        .and_then(|value| value.as_str())
        .ok_or("is missing remote workspace")?;
    let materialization_mode = lab
        .get("sync_mode")
        .and_then(|value| value.as_str())
        .ok_or("is missing materialization mode")?;
    let lab_snapshot = lab
        .get("source_snapshot")
        .ok_or("is missing source snapshot evidence")?;

    if snapshot.sync_mode != LAB_SOURCE_SNAPSHOT_SYNC_MODE {
        return Err(format!(
            "has untrusted source mode `{}`",
            snapshot.sync_mode
        ));
    }
    if !matches!(materialization_mode, "git" | "snapshot" | "snapshot-git") {
        return Err(format!(
            "has untrusted workspace materialization mode `{materialization_mode}`"
        ));
    }
    if materialization_mode == "git"
        && snapshot
            .sync_excludes
            .iter()
            .any(|exclude| exclude == ".git" || exclude == ".git/")
    {
        return Err("claims git materialization while excluding .git metadata".to_string());
    }
    if snapshot.dirty {
        return Err("records a dirty source checkout".to_string());
    }
    if !is_git_revision(source_revision) {
        return Err("has an invalid source revision".to_string());
    }
    if snapshot.runner_id.trim().is_empty() || snapshot.runner_id != runner_id {
        return Err("runner identity does not match the Lab dispatch".to_string());
    }
    if !paths_equal(
        expected_remote_component_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        recorded_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        lab_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) {
        return Err("remote workspace does not match materialized path".to_string());
    }
    if lab.get("status").and_then(|value| value.as_str()) != Some("offloaded") {
        return Err("dispatch status is not `offloaded`".to_string());
    }
    if serde_json::to_value(&snapshot).ok().as_ref() != Some(lab_snapshot) {
        return Err("source snapshot does not match Lab dispatch evidence".to_string());
    }
    let verification = lab.get("workspace_verification");
    let (expected_content_hash, verification_identity) = match verification {
        Some(verification) => {
            if verification.get("schema").and_then(|value| value.as_str())
                != Some("homeboy/lab-workspace-verification/v1")
            {
                return Err("has an unsupported workspace verification schema".to_string());
            }
            let identity = verification
                .get("identity")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace verification identity")?;
            let content_hash = verification
                .get("content_hash")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace verification content hash")?;
            let excludes = verification
                .get("sync_excludes")
                .ok_or("is missing workspace verification sync excludes")?;
            if excludes != &serde_json::json!(snapshot.sync_excludes) {
                return Err("sync excludes do not match workspace verification".to_string());
            }
            if verification.get("source_snapshot") != Some(lab_snapshot) {
                return Err("source snapshot does not match workspace verification".to_string());
            }
            let primary_workspace = verification
                .get("primary_workspace")
                .ok_or("is missing workspace verification primary workspace")?;
            if primary_workspace
                .get("identity")
                .and_then(|value| value.as_str())
                != Some(identity)
                || primary_workspace
                    .get("remote_path")
                    .and_then(|value| value.as_str())
                    != Some(recorded_remote_path)
            {
                return Err("primary workspace does not match workspace verification".to_string());
            }
            (content_hash, identity)
        }
        None if materialization_mode == "git" => {
            let content_hash = lab
                .get("workspace_content_hash")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace content hash")?;
            let identity = lab
                .get("workspace_materialization_plan")
                .and_then(|value| value.get("identity"))
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace materialization identity")?;
            (content_hash, identity)
        }
        None => return Err("is missing workspace verification metadata".to_string()),
    };
    if workspace_identity != verification_identity {
        return Err("workspace identity does not match workspace verification".to_string());
    }
    let actual_content_hash =
        workspace_content_hash(materialized_workspace_path, &snapshot.sync_excludes)
            .map_err(|error| format!("could not hash materialized workspace: {}", error.message))?;
    if actual_content_hash != expected_content_hash {
        return Err("content hash does not match the controller materialization".to_string());
    }

    Ok(VerifiedLabWorkspaceProvenance {
        source_revision: source_revision.to_string(),
        materialization_mode: materialization_mode.to_string(),
        runner_id: snapshot.runner_id,
        workspace_identity: workspace_identity.to_string(),
    })
}

fn env_json<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    std::env::var(name)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn paths_equal(left: &str, right: &str) -> bool {
    matches!((Path::new(left).canonicalize(), Path::new(right).canonicalize()), (Ok(left), Ok(right)) if left == right)
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
