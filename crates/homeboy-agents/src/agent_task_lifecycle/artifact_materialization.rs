use std::fs;
use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_task::AgentTaskArtifact;
use homeboy_core::{Error, Result};

use super::{
    apply_aggregate_to_record, status, store, verified_controller_artifact_projection_path,
};

/// Materialize recovered patch artifacts for a controller-side promotion.
///
/// Controller-retained projections are authoritative for controller-local
/// execution. Remote retrieval is only available to artifacts that retain a
/// complete runner/job provenance binding.
pub fn materialize_recovered_patch_artifact(
    run_id: &str,
    task_id: Option<&str>,
    artifact_id: Option<&str>,
) -> Result<bool> {
    let mut record = status(run_id)?;
    let mut aggregate = store::read_aggregate(&record.run_id)?;
    let artifact_id = artifact_id
        .map(|artifact_id| resolve_promotion_patch_artifact_id(run_id, task_id, artifact_id))
        .transpose()?;
    let mut changed = false;
    for outcome in &mut aggregate.outcomes {
        if task_id.is_some_and(|expected| expected != outcome.task_id) {
            continue;
        }
        for artifact in &mut outcome.artifacts {
            if artifact_id
                .as_deref()
                .is_some_and(|expected| expected != artifact.id)
                || !is_patch_artifact_kind(&artifact.kind)
                || !is_recovered_artifact(artifact, record.runner_id().zip(record.runner_job_id()))
            {
                continue;
            }
            changed |= materialize_artifact(
                artifact,
                &record.run_id,
                &outcome.task_id,
                record.runner_id().zip(record.runner_job_id()),
            )?;
        }
        let finalized = outcome.artifacts.clone();
        for typed in &mut outcome.typed_artifacts {
            if let Some(artifact) = &mut typed.artifact {
                if let Some(finalized) = finalized.iter().find(|value| value.id == artifact.id) {
                    *artifact = finalized.clone();
                }
            }
        }
    }
    if changed {
        let aggregate_path = store::aggregate_path(&record.run_id)?.display().to_string();
        let plan = super::load_plan(&record.run_id)?;
        apply_aggregate_to_record(&mut record, &plan, &aggregate, aggregate_path);
        store::write_aggregate_and_record(&record, &aggregate)?;
    }
    Ok(changed)
}

/// Resolve a persisted artifact record id to the lifecycle artifact id used by
/// promotion. Controller mirrors and runner references intentionally have
/// distinct record ids while sharing this logical id.
pub fn resolve_promotion_patch_artifact_id(
    run_id: &str,
    task_id: Option<&str>,
    artifact_id: &str,
) -> Result<String> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let mut logical_ids = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|artifact| is_patch_artifact_kind(&artifact.kind))
        .filter(|artifact| {
            task_id.is_none_or(|task_id| {
                artifact
                    .metadata_json
                    .pointer("/agent_task/task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task_id)
            })
        })
        .filter_map(|artifact| {
            let logical_id = artifact
                .metadata_json
                .pointer("/agent_task/logical_artifact_id")
                .and_then(serde_json::Value::as_str)?;
            (artifact.id == artifact_id || logical_id == artifact_id)
                .then(|| logical_id.to_string())
        })
        .collect::<Vec<_>>();
    logical_ids.sort();
    logical_ids.dedup();

    match logical_ids.as_slice() {
        [] => Ok(artifact_id.to_string()),
        [logical_id] => Ok(logical_id.clone()),
        _ => Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "persisted artifact id '{artifact_id}' matches multiple logical patch artifacts for run '{run_id}'"
            ),
            Some(artifact_id.to_string()),
            None,
        )),
    }
}

fn is_patch_artifact_kind(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "patch" | "diff" | "git-diff" | "git_diff" | "workspace_patch" | "workspace-patch"
    )
}

fn is_recovered_artifact(
    artifact: &AgentTaskArtifact,
    runner_binding: Option<(&str, &str)>,
) -> bool {
    runner_binding.is_some()
        || artifact
            .url
            .as_deref()
            .is_some_and(|url| url.starts_with("homeboy://agent-task/run/"))
        || artifact
            .path
            .as_deref()
            .is_some_and(|path| path.starts_with("runner-artifact://"))
}

fn materialize_artifact(
    artifact: &mut AgentTaskArtifact,
    run_id: &str,
    task_id: &str,
    runner_binding: Option<(&str, &str)>,
) -> Result<bool> {
    if let Some(path) = artifact
        .path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        validate_file(artifact, &path)?;
        return Ok(false);
    }

    if let Some(path) = verified_controller_artifact_projection_path(run_id, task_id, artifact)? {
        validate_file(artifact, &path)?;
        artifact.path = Some(path.display().to_string());
        set_materialization_metadata(artifact, "controller_retained_bytes", None);
        return Ok(true);
    }

    // The record-level runner binding is the primary provenance, but it can be
    // dropped when the controller session is lost and re-established across a
    // reconnect. The persisted artifact still carries its own runner provenance
    // (`metadata.source_provenance.runner_id`), which is all the download ref
    // needs, so fall back to it to let a reconnected authenticated runner supply
    // the bytes. `validate_file` then re-verifies size + SHA-256 against the
    // aggregate, so a mismatched or substituted artifact still fails closed
    // (#8762).
    let (runner_id, runner_job_id) = match runner_binding {
        Some((runner_id, runner_job_id)) => {
            (runner_id.to_string(), Some(runner_job_id.to_string()))
        }
        None => {
            let runner_id = artifact_provenance_runner_id(artifact).ok_or_else(|| {
                unavailable(
                    artifact,
                    "no authenticated runner/job binding exists, no artifact runner provenance is recorded, and no verified controller-retained bytes are available",
                )
            })?;
            (runner_id, None)
        }
    };
    let remote_ref = homeboy_core::execution_contract::EXECUTION_CONTRACT
        .artifacts
        .runner_artifact_ref(&runner_id, run_id, &artifact.id);
    let path = materialized_path(run_id, task_id, &artifact.id)?;
    let download = homeboy_core::observation::runs_service::with_runner_evidence(|provider| {
        provider.download_remote_artifact(&remote_ref, Some(path.clone()))
    })?;
    validate_file(artifact, &download.output_path)?;
    artifact.path = Some(download.output_path.display().to_string());
    set_materialization_metadata(
        artifact,
        if runner_job_id.is_some() {
            "authenticated_runner_artifact"
        } else {
            "reconnected_runner_artifact_provenance"
        },
        Some((runner_id.as_str(), runner_job_id.as_deref())),
    );
    Ok(true)
}

/// The runner that produced this artifact, recorded on the artifact itself
/// (`metadata.source_provenance.runner_id`) so it survives the loss of the
/// record-level runner/job binding across a controller reconnect. Empty or
/// missing provenance yields `None` so the caller fails closed.
fn artifact_provenance_runner_id(artifact: &AgentTaskArtifact) -> Option<String> {
    artifact
        .metadata
        .pointer("/source_provenance/runner_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|runner_id| !runner_id.is_empty())
        .map(str::to_string)
}

fn materialized_path(run_id: &str, task_id: &str, artifact_id: &str) -> Result<PathBuf> {
    crate::agent_task_provider::artifact_finalization::validate_token("artifact.id", artifact_id)?;
    Ok(homeboy_core::paths::artifact_root()?
        .join("recovered-runner-artifacts")
        .join(homeboy_core::paths::sanitize_path_segment(run_id))
        .join(homeboy_core::paths::sanitize_path_segment(task_id))
        .join(artifact_id))
}

fn validate_file(artifact: &AgentTaskArtifact, path: &PathBuf) -> Result<()> {
    let bytes = fs::read(path).map_err(io_error(path))?;
    if artifact.size_bytes != Some(bytes.len() as u64) {
        return Err(unavailable(
            artifact,
            "retained artifact size does not match the aggregate",
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if artifact.sha256.as_deref() != Some(sha256.as_str()) {
        return Err(unavailable(
            artifact,
            "retained artifact SHA-256 does not match the aggregate",
        ));
    }
    Ok(())
}

fn set_materialization_metadata(
    artifact: &mut AgentTaskArtifact,
    source: &str,
    runner_binding: Option<(&str, Option<&str>)>,
) {
    if !artifact.metadata.is_object() {
        artifact.metadata = json!({});
    }
    artifact.metadata["controller_artifact_materialization"] = json!({
        "source": source,
        "runner_id": runner_binding.map(|binding| binding.0),
        "runner_job_id": runner_binding.and_then(|binding| binding.1),
    });
}

fn unavailable(artifact: &AgentTaskArtifact, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "artifact",
        format!(
            "recovered runner artifact `{}` cannot be materialized: {reason}",
            artifact.id
        ),
        Some(artifact.id.clone()),
        None,
    )
}

fn io_error(path: &PathBuf) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("materialize recovered artifact {}", path.display())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(bytes: &[u8]) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: None,
            url: Some(
                "homeboy://agent-task/run/local/artifacts#task=task&artifact=patch".to_string(),
            ),
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            metadata: json!({ "executor_artifact_finalized": true }),
        }
    }

    #[test]
    fn controller_retained_bytes_materialize_without_a_runner_binding() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let bytes = b"patch bytes";
            let source = tempfile::NamedTempFile::new().expect("source");
            fs::write(source.path(), bytes).expect("write source");
            let artifact = artifact(bytes);
            let store =
                homeboy_core::observation::ObservationStore::open_initialized().expect("store");
            store
                .upsert_imported_run(&homeboy_core::observation::RunRecord {
                    id: "local".to_string(),
                    kind: "agent-task".to_string(),
                    component_id: None,
                    started_at: "2026-07-16T00:00:00Z".to_string(),
                    finished_at: None,
                    status: "pass".to_string(),
                    command: None,
                    cwd: None,
                    homeboy_version: None,
                    git_sha: None,
                    rig_id: None,
                    metadata_json: json!({}),
                })
                .expect("run");
            let projected = store
                .record_artifact_with_id(
                    "local",
                    "patch",
                    source.path(),
                    "controller-patch",
                    json!({ "agent_task": { "task_id": "task", "logical_artifact_id": "patch" } }),
                )
                .expect("projection");

            let mut artifact = artifact;
            assert!(
                materialize_artifact(&mut artifact, "local", "task", None).expect("materialize")
            );
            assert_eq!(artifact.path.as_deref(), Some(projected.path.as_str()));
            assert_eq!(
                artifact.metadata["controller_artifact_materialization"]["source"],
                "controller_retained_bytes"
            );
        });
    }

    #[test]
    fn runner_artifact_without_a_job_binding_fails_closed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let error = materialize_artifact(&mut artifact(b"patch bytes"), "run", "task", None)
                .expect_err("runner artifact without a binding must not download");
            assert!(error
                .message
                .contains("no authenticated runner/job binding exists"));
        });
    }

    #[test]
    fn artifact_runner_provenance_authorizes_download_without_a_record_binding() {
        // #8762: after a reconnect the record-level runner/job binding can be
        // lost, but the artifact still carries its own runner provenance. That
        // provenance must authorize a reconnected authenticated runner to supply
        // the bytes rather than failing closed at the binding gate.
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut artifact = artifact(b"patch bytes");
            artifact.metadata = json!({ "source_provenance": { "runner_id": "homeboy-lab" } });

            let error = materialize_artifact(&mut artifact, "run", "task", None)
                .expect_err("no runner evidence provider is registered in this unit test");

            // The fallback got PAST the fail-closed binding gate and reached the
            // download; without a registered provider it fails at download, not
            // at "no authenticated runner/job binding".
            assert!(
                !error
                    .message
                    .contains("no authenticated runner/job binding exists"),
                "provenance fallback must not fail at the binding gate: {}",
                error.message
            );
            assert!(
                error.message.contains("runner evidence provider"),
                "must reach the runner download path: {}",
                error.message
            );
        });
    }

    #[test]
    fn empty_or_missing_artifact_provenance_still_fails_closed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut artifact = artifact(b"patch bytes");
            artifact.metadata = json!({ "source_provenance": { "runner_id": "   " } });

            let error = materialize_artifact(&mut artifact, "run", "task", None)
                .expect_err("blank provenance must fail closed");
            assert!(error
                .message
                .contains("no artifact runner provenance is recorded"));
        });
    }
}
