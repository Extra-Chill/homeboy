use std::fs;
use std::path::PathBuf;

use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_task::AgentTaskArtifact;
use crate::{Error, Result};

use super::{apply_aggregate_to_record, status, store};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RunnerArtifactRoute {
    runner_id: String,
    job_id: String,
}

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
            let route = resolve_runner_artifact_route(&record, artifact)?;
            changed |= materialize_artifact(
                artifact,
                &route.runner_id,
                &route.job_id,
                &record.run_id,
                &outcome.task_id,
                |id| {
                    crate::observation::runs_service::runner_evidence::with_runner_evidence(|p| {
                        p.runner_artifact_content(&route.runner_id, &route.job_id, id)
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

fn resolve_runner_artifact_route(
    record: &super::AgentTaskRunRecord,
    artifact: &AgentTaskArtifact,
) -> Result<RunnerArtifactRoute> {
    let lifecycle_route =
        record
            .runner_id()
            .zip(record.runner_job_id())
            .map(|(runner_id, job_id)| RunnerArtifactRoute {
                runner_id: runner_id.to_string(),
                job_id: job_id.to_string(),
            });
    let mirrored_identities = if lifecycle_route.is_none() {
        crate::observation::runs_service::mirrored_runner_job_identities(&record.run_id)?
    } else {
        Vec::new()
    };
    resolve_runner_artifact_route_from_identities(artifact, lifecycle_route, mirrored_identities)
}

fn resolve_runner_artifact_route_from_identities(
    artifact: &AgentTaskArtifact,
    lifecycle_route: Option<RunnerArtifactRoute>,
    mirrored_identities: Vec<(String, String)>,
) -> Result<RunnerArtifactRoute> {
    let mut routes = lifecycle_route.into_iter().collect::<Vec<_>>();
    if routes.is_empty() {
        routes = mirrored_identities
            .into_iter()
            .map(|(runner_id, job_id)| RunnerArtifactRoute { runner_id, job_id })
            .collect();
    }
    routes.sort();
    routes.dedup();
    match routes.as_slice() {
        [route] => Ok(route.clone()),
        [] => Err(unavailable(artifact, "no authenticated runner/job binding exists in lifecycle or mirrored runner evidence")),
        _ => Err(unavailable(artifact, "multiple conflicting authenticated runner/job bindings exist in lifecycle or mirrored runner evidence")),
    }
}

fn materialize_artifact(
    artifact: &mut AgentTaskArtifact,
    runner_id: &str,
    job_id: &str,
    run_id: &str,
    task_id: &str,
    fetch: impl FnOnce(&str) -> Result<serde_json::Value>,
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
    let response = fetch(&artifact.id)?;
    let encoded = response
        .get("content_base64")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            unavailable(
                artifact,
                "runner job result has no mirrored artifact content",
            )
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "artifact",
                format!("runner artifact content is not valid base64: {error}"),
                None,
                None,
            )
        })?;
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
        "runner_job_id": job_id,
        "artifact_id": artifact.id,
        "source": "runner_terminal_artifact_content",
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
    fn route_selection_prefers_lifecycle_and_fails_closed_for_missing_or_ambiguous_mirrors() {
        let value = artifact(b"patch bytes");
        let lifecycle = RunnerArtifactRoute {
            runner_id: "lifecycle-runner".to_string(),
            job_id: "lifecycle-job".to_string(),
        };
        assert_eq!(
            resolve_runner_artifact_route_from_identities(
                &value,
                Some(lifecycle.clone()),
                vec![("mirror-runner".to_string(), "mirror-job".to_string())],
            )
            .expect("lifecycle route wins"),
            lifecycle
        );
        assert_eq!(
            resolve_runner_artifact_route_from_identities(
                &value,
                None,
                vec![("mirror-runner".to_string(), "mirror-job".to_string())],
            )
            .expect("single mirror route"),
            RunnerArtifactRoute {
                runner_id: "mirror-runner".to_string(),
                job_id: "mirror-job".to_string(),
            }
        );

        let missing = resolve_runner_artifact_route_from_identities(&value, None, Vec::new())
            .expect_err("missing route fails closed");
        assert_eq!(missing.code, crate::ErrorCode::ValidationInvalidArgument);
        assert!(missing
            .message
            .contains("no authenticated runner/job binding"));

        let ambiguous = resolve_runner_artifact_route_from_identities(
            &value,
            None,
            vec![
                ("runner-a".to_string(), "job-a".to_string()),
                ("runner-b".to_string(), "job-b".to_string()),
            ],
        )
        .expect_err("ambiguous routes fail closed");
        assert_eq!(ambiguous.code, crate::ErrorCode::ValidationInvalidArgument);
        assert!(ambiguous
            .message
            .contains("multiple conflicting authenticated runner/job bindings"));
    }

    #[test]
    fn selected_mirrored_route_materializes_verified_controller_bytes() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let bytes = b"mirrored patch bytes";
        let mut value = artifact(bytes);
        let route = resolve_runner_artifact_route_from_identities(
            &value,
            None,
            vec![("mirror-runner".to_string(), "mirror-job".to_string())],
        )
        .expect("select mirrored route");

        assert!(materialize_artifact(
            &mut value,
            &route.runner_id,
            &route.job_id,
            "run",
            "task",
            |_| Ok(
                json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) })
            ),
        )
        .expect("materialize mirrored artifact"));
        let path = PathBuf::from(value.path.as_deref().expect("controller path"));
        assert_eq!(std::fs::read(path).expect("controller bytes"), bytes);
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["runner_id"],
            "mirror-runner"
        );
        assert_eq!(
            value.metadata["controller_artifact_materialization"]["runner_job_id"],
            "mirror-job"
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
        assert!(
            materialize_artifact(&mut value, "lab", "job", "run", "task", |_| Ok(
                json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) })
            ),)
            .expect("materialize")
        );
        assert!(
            !materialize_artifact(&mut value, "lab", "job", "run", "task", |_| panic!(
                "must reuse controller artifact"
            ),)
            .expect("replay")
        );
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
    }

    #[test]
    fn rejects_runner_bytes_with_wrong_integrity_metadata() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let mut value = artifact(b"expected");
        let error = materialize_artifact(&mut value, "lab", "job", "run", "task", |_| Ok(json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(b"wrong") }))).expect_err("integrity failure");
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

        assert!(
            !materialize_artifact(&mut value, "lab", "job", "run", "task", |_| panic!(
                "must preserve a usable local artifact"
            ),)
            .expect("local artifact")
        );
        assert_eq!(value.path.as_deref(), source.to_str());
    }
}
