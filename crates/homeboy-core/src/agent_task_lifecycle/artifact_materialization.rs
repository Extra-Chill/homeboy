use std::fs;
use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_task::AgentTaskArtifact;
use crate::{Error, Result};

use super::{apply_aggregate_to_record, status, store};

/// Materialize a recovered reverse-runner patch into controller-owned storage.
/// The aggregate is updated atomically with the durable run record, making a
/// replay reuse the verified local file instead of contacting the runner again.
pub fn materialize_recovered_patch_artifact(
    run_id: &str,
    task_id: Option<&str>,
    artifact_id: Option<&str>,
) -> Result<bool> {
    let mut record = status(run_id)?;
    let mut aggregate = store::read_aggregate(&record.run_id)?;
    let mut changed = false;
    for outcome in &mut aggregate.outcomes {
        if task_id.is_some_and(|expected| expected != outcome.task_id) {
            continue;
        }
        for artifact in &mut outcome.artifacts {
            if artifact_id.is_some_and(|expected| expected != artifact.id)
                || !matches!(artifact.kind.as_str(), "patch" | "diff" | "workspace_patch")
            {
                continue;
            }
            let runner_id = record.runner_id().ok_or_else(|| {
                unavailable(
                    artifact,
                    "no validated source-provenance runner is recorded for the aggregate",
                )
            })?;
            let remote_ref = canonical_runner_artifact_ref(
                runner_id,
                &record.run_id,
                &outcome.task_id,
                artifact,
            )?;
            changed |= materialize_artifact(
                artifact,
                runner_id,
                &remote_ref,
                &record.run_id,
                &outcome.task_id,
                |remote_ref| {
                    crate::observation::runs_service::runner_evidence::with_runner_evidence(|p| {
                        p.download_remote_artifact(remote_ref, None)
                    })
                    .and_then(|download| {
                        fs::read(&download.output_path).map_err(io_error(&download.output_path))
                    })
                },
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

fn canonical_runner_artifact_ref(
    runner_id: &str,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> Result<String> {
    use crate::execution_contract::encode_uri_component;

    let canonical_url = format!(
        "homeboy://agent-task/run/{}/artifacts#task={}&artifact={}",
        encode_uri_component(run_id),
        encode_uri_component(task_id),
        encode_uri_component(&artifact.id),
    );
    if artifact.url.as_deref() != Some(canonical_url.as_str()) {
        return Err(unavailable(
            artifact,
            "aggregate does not retain its canonical agent-task artifact route",
        ));
    }
    Ok(crate::execution_contract::EXECUTION_CONTRACT
        .artifacts
        .runner_artifact_ref(runner_id, run_id, &artifact.id))
}

fn materialize_artifact(
    artifact: &mut AgentTaskArtifact,
    runner_id: &str,
    remote_ref: &str,
    run_id: &str,
    task_id: &str,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>>,
) -> Result<bool> {
    if let Some(path) = artifact
        .path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        validate_bytes(artifact, &fs::read(&path).map_err(io_error(&path))?)?;
        return Ok(false);
    }
    let expected_size = artifact
        .size_bytes
        .ok_or_else(|| unavailable(artifact, "missing durable byte size"))?;
    let expected_sha = artifact
        .sha256
        .as_deref()
        .ok_or_else(|| unavailable(artifact, "missing durable SHA-256"))?;
    let path = materialized_path(run_id, task_id, &artifact.id, expected_sha)?;
    if path.is_file() {
        validate_bytes(artifact, &fs::read(&path).map_err(io_error(&path))?)?;
        artifact.path = Some(path.display().to_string());
        return Ok(false);
    }
    let bytes = fetch(remote_ref)?;
    validate_bytes(artifact, &bytes)?;
    fs::create_dir_all(path.parent().expect("materialized artifact parent"))
        .map_err(io_error(path.parent().unwrap()))?;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&temporary, &bytes).map_err(io_error(&temporary))?;
    fs::rename(&temporary, &path).map_err(io_error(&path))?;
    artifact.path = Some(path.display().to_string());
    if !artifact.metadata.is_object() {
        artifact.metadata = json!({});
    }
    artifact.metadata["controller_artifact_materialization"] = json!({
        "runner_id": runner_id,
        "artifact_id": artifact.id,
        "source": "runner_canonical_artifact_content",
        "source_ref": remote_ref,
        "verified_size_bytes": expected_size,
        "verified_sha256": expected_sha,
    });
    Ok(true)
}

fn materialized_path(
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
    sha256: &str,
) -> Result<PathBuf> {
    crate::agent_task_provider::artifact_finalization::validate_token("artifact.id", artifact_id)?;
    Ok(crate::paths::artifact_root()?
        .join("recovered-runner-artifacts")
        .join(crate::paths::sanitize_path_segment(run_id))
        .join(crate::paths::sanitize_path_segment(task_id))
        .join(format!("{artifact_id}-{sha256}")))
}

fn validate_bytes(artifact: &AgentTaskArtifact, bytes: &[u8]) -> Result<()> {
    let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if artifact.size_bytes != Some(actual_size) {
        return Err(Error::validation_invalid_argument(
            "artifact",
            format!(
                "recovered artifact size mismatch: expected {:?}, got {actual_size}",
                artifact.size_bytes
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    let actual_sha = format!("{:x}", Sha256::digest(bytes));
    if artifact.sha256.as_deref() != Some(actual_sha.as_str()) {
        return Err(Error::validation_invalid_argument(
            "artifact",
            format!(
                "recovered artifact SHA-256 mismatch: expected {:?}, got {actual_sha}",
                artifact.sha256
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(())
}

fn unavailable(artifact: &AgentTaskArtifact, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "artifact",
        format!("recovered runner artifact `{}` cannot be materialized: {reason}", artifact.id),
        Some(artifact.id.clone()),
        Some(vec!["The source runner/job must retain mirrored artifact bytes; inspect `homeboy runner job artifacts <runner-id> <job-id> <artifact-id>`.".to_string()]),
    )
}

fn io_error(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
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
    use std::sync::{Mutex, OnceLock};

    fn artifact_root_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn artifact(bytes: &[u8]) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            metadata: json!({}),
        }
    }

    #[test]
    fn canonical_route_requires_the_aggregate_artifact_identity() {
        let value = artifact(b"patch bytes");
        assert_eq!(
            canonical_runner_artifact_ref("runner", "run", "task", &value)
                .expect_err("missing canonical route")
                .message,
            "Invalid argument 'artifact': recovered runner artifact `patch` cannot be materialized: aggregate does not retain its canonical agent-task artifact route"
        );
        let mut value = value;
        value.url =
            Some("homeboy://agent-task/run/run/artifacts#task=task&artifact=patch".to_string());
        assert_eq!(
            canonical_runner_artifact_ref("runner", "run", "task", &value)
                .expect("canonical runner ref"),
            "runner-artifact://runner/run/patch"
        );
    }

    #[test]
    fn selected_mirrored_route_materializes_verified_controller_bytes() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let bytes = b"mirrored patch bytes";
        let mut value = artifact(bytes);
        value.url =
            Some("homeboy://agent-task/run/run/artifacts#task=task&artifact=patch".to_string());
        let remote_ref = canonical_runner_artifact_ref("mirror-runner", "run", "task", &value)
            .expect("canonical route");

        assert!(materialize_artifact(
            &mut value,
            "mirror-runner",
            &remote_ref,
            "run",
            "task",
            |_| Ok(bytes.to_vec()),
        )
        .expect("materialize mirrored artifact"));
        let path = PathBuf::from(value.path.as_deref().expect("controller path"));
        assert_eq!(std::fs::read(path).expect("controller bytes"), bytes);
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["runner_id"],
            "mirror-runner"
        );
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["source"],
            "runner_canonical_artifact_content"
        );
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["verified_size_bytes"],
            bytes.len()
        );
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["verified_sha256"],
            value.sha256.as_deref().expect("SHA")
        );
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
    }

    #[test]
    fn materializes_valid_runner_bytes_and_replays_without_fetching() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let bytes = b"patch bytes";
        let mut value = artifact(bytes);
        assert!(materialize_artifact(
            &mut value,
            "lab",
            "runner-artifact://lab/run/patch",
            "run",
            "task",
            |_| Ok(bytes.to_vec()),
        )
        .expect("materialize"));
        assert!(!materialize_artifact(
            &mut value,
            "lab",
            "runner-artifact://lab/run/patch",
            "run",
            "task",
            |_| panic!("must reuse controller artifact"),
        )
        .expect("replay"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
    }

    #[test]
    fn rejects_runner_bytes_with_wrong_integrity_metadata() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let mut value = artifact(b"expected");
        let error = materialize_artifact(
            &mut value,
            "lab",
            "runner-artifact://lab/run/patch",
            "run",
            "task",
            |_| Ok(b"wrong".to_vec()),
        )
        .expect_err("integrity failure");
        assert!(error.message.contains("mismatch"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
    }

    #[test]
    fn preserves_an_existing_local_provider_artifact() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("patch");
        std::fs::write(&source, b"local patch").expect("patch");
        let mut value = artifact(b"local patch");
        value.path = Some(source.display().to_string());

        assert!(!materialize_artifact(
            &mut value,
            "lab",
            "runner-artifact://lab/run/patch",
            "run",
            "task",
            |_| panic!("must preserve a usable local artifact"),
        )
        .expect("local artifact"));
        assert_eq!(value.path.as_deref(), source.to_str());
    }
}
