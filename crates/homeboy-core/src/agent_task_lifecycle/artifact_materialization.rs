use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
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
    let Some(runner_id) = record.runner_id().filter(|value| !value.trim().is_empty()) else {
        return Ok(false);
    };
    let Some(job_id) = record
        .runner_job_id()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(false);
    };
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
            let typed_path = outcome.typed_artifacts.iter().find_map(|typed| {
                let typed_artifact = typed.artifact.as_ref()?;
                (typed_artifact.id == artifact.id
                    && typed_artifact.size_bytes == artifact.size_bytes
                    && typed_artifact.sha256 == artifact.sha256)
                    .then(|| {
                        typed_artifact.path.clone().or_else(|| {
                            typed
                                .payload
                                .get("path")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        })
                    })
                    .flatten()
            });
            changed |= materialize_artifact(
                artifact,
                &runner_id,
                &job_id,
                &record.run_id,
                &outcome.task_id,
                |id| crate::runner::runner_artifact_content(&runner_id, &job_id, id),
                typed_path.as_deref(),
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

fn materialize_artifact(
    artifact: &mut AgentTaskArtifact,
    runner_id: &str,
    job_id: &str,
    run_id: &str,
    task_id: &str,
    fetch: impl FnOnce(&str) -> Result<serde_json::Value>,
    typed_path: Option<&str>,
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
    let response = match fetch(&artifact.id) {
        Ok(response) => response,
        Err(error) => match typed_path {
            Some(path) => legacy_direct_typed_artifact_content(runner_id, artifact, path)?,
            None => return Err(error),
        },
    };
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

/// Legacy Lab terminal results can describe finalized files only in their typed
/// aggregate. Capture those files through the managed `runs artifact attach`
/// source primitive after checking the remote path against runner-owned roots.
fn legacy_direct_typed_artifact_content(
    runner_id: &str,
    artifact: &AgentTaskArtifact,
    path: &str,
) -> Result<serde_json::Value> {
    let status = crate::runner::status(runner_id)?;
    if status.session.as_ref().map(|session| &session.mode)
        != Some(&crate::runner::RunnerTunnelMode::DirectSsh)
    {
        return Err(Error::validation_invalid_argument(
            "artifact.path",
            "legacy typed artifact fallback is only available through a direct runner session",
            Some(path.to_string()),
            None,
        ));
    }
    let runner = crate::runner::load(runner_id)?;
    authorize_legacy_runner_path(&runner, artifact, path)?;
    let source =
        crate::observation::runner_artifact_attach::copy_runner_artifact_source(&runner, path)?;
    let bytes = fs::read(&source.path).map_err(io_error(&source.path))?;
    crate::observation::runner_artifact_attach::cleanup_runner_attach_source(&source);
    Ok(json!({
        "command": "runs.artifact.attach.legacy_typed_path",
        "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

fn authorize_legacy_runner_path(
    runner: &crate::runner::Runner,
    artifact: &AgentTaskArtifact,
    path: &str,
) -> Result<()> {
    let mut roots = runner
        .workspace_root
        .iter()
        .chain(runner.policy.workspace_roots.iter())
        .map(|root| crate::paths::normalize_remote_root(root))
        .collect::<Vec<_>>();
    if let Some(root) = runner.env.get("HOMEBOY_ARTIFACT_ROOT") {
        roots.push(crate::paths::normalize_remote_root(root));
    }
    if let Some(root) = finalized_artifact_root(artifact, path) {
        roots.push(root);
    }
    roots.sort();
    roots.dedup();
    crate::paths::authorize_remote_artifact_path(
        Path::new(path),
        &roots,
        crate::paths::RemotePathRootContainment::RemoteString,
    )
    .map_err(|error| Error::validation_invalid_argument(
        "artifact.path",
        format!("legacy typed runner artifact path is not authorized: {error:?}"),
        Some(path.to_string()),
        Some(vec!["The path must be absolute, traversal-free, and beneath a configured runner workspace or HOMEBOY_ARTIFACT_ROOT.".to_string()]),
    ))
}

fn finalized_artifact_root(artifact: &AgentTaskArtifact, path: &str) -> Option<String> {
    let path = Path::new(path);
    let file = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let finalized = parent.parent()?;
    let artifacts = finalized.parent()?;
    if finalized.file_name()?.to_str()? != "executor-finalized"
        || artifacts.file_name()?.to_str()? != "artifacts"
        || uuid::Uuid::parse_str(parent.file_name()?.to_str()?).is_err()
    {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(artifact.kind.as_bytes());
    hash.update([0]);
    hash.update(artifact.id.as_bytes());
    (file == format!("artifact-{:x}", hash.finalize())).then(|| artifacts.display().to_string())
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
    fn materializes_valid_runner_bytes_and_replays_without_fetching() {
        let _lock = artifact_root_lock().lock().expect("artifact root lock");
        let root = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", root.path());
        let bytes = b"patch bytes";
        let mut value = artifact(bytes);
        assert!(materialize_artifact(
            &mut value,
            "lab",
            "job",
            "run",
            "task",
            |_| Ok(
                json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) })
            ),
            None
        )
        .expect("materialize"));
        assert!(!materialize_artifact(
            &mut value,
            "lab",
            "job",
            "run",
            "task",
            |_| panic!("must reuse controller artifact"),
            None
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
        let error = materialize_artifact(&mut value, "lab", "job", "run", "task", |_| Ok(json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(b"wrong") })), None).expect_err("integrity failure");
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
            "job",
            "run",
            "task",
            |_| panic!("must preserve a usable local artifact"),
            None
        )
        .expect("local artifact"));
        assert_eq!(value.path.as_deref(), source.to_str());
    }

    #[test]
    fn recognizes_a_legacy_finalized_typed_artifact_path() {
        let value = artifact(b"patch");
        let mut hash = Sha256::new();
        hash.update(b"patch\0patch");
        let path = format!(
            "/home/runner/.local/share/homeboy/artifacts/executor-finalized/41e5fcb2-fbc4-4a55-b745-377684e3e16f/artifact-{:x}",
            hash.finalize()
        );
        assert_eq!(
            finalized_artifact_root(&value, &path).as_deref(),
            Some("/home/runner/.local/share/homeboy/artifacts")
        );
    }

    #[test]
    fn rejects_traversal_or_noncanonical_finalized_typed_paths() {
        let value = artifact(b"patch");
        assert!(finalized_artifact_root(
            &value,
            "/home/runner/.local/share/homeboy/artifacts/executor-finalized/../secret"
        )
        .is_none());
        assert!(finalized_artifact_root(
            &value,
            "/tmp/artifacts/executor-finalized/41e5fcb2-fbc4-4a55-b745-377684e3e16f/artifact-forged"
        )
        .is_none());
    }
}
