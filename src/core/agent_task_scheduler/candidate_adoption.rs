use std::io::Write;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use super::*;
use crate::core::agent_task_promotion::{normalize_promotion_patch, validate_artifact_content};

pub(super) fn select_candidate_adoption(
    template: &AgentTaskCandidateAdoption,
    artifacts: &[AgentTaskArtifact],
    running: &RunningTask,
) -> Result<AgentTaskCandidateAdoption, String> {
    let matches = artifacts
        .iter()
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("candidate adoption requires exactly one matching artifact id".to_string());
    }
    let artifact = matches[0];
    let expected_run = running.run_id.as_deref().unwrap_or_default();
    let expected_base = running.task_base_sha.as_deref().unwrap_or_default();
    let repository_identity = artifact.metadata["repository_identity"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "candidate artifact has no canonical repository provenance".to_string())?;
    let workspace_identity = artifact.metadata["workspace_identity"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "candidate artifact has no canonical workspace provenance".to_string())?;
    if artifact.kind != "patch"
        || artifact.metadata["run_id"] != expected_run
        || artifact.metadata["task_id"] != running.task_id
        || artifact.metadata["producer_attempt"] != running.attempt
        || artifact.metadata["base_ref"] != expected_base
        || artifact.metadata["provider_backend"] != running.request.executor.backend
        || artifact.metadata["provider_selector"]
            != serde_json::json!(running.request.executor.selector)
        || artifact.metadata["provider_model"]
            != serde_json::json!(running.request.executor.model())
    {
        return Err(
            "candidate adoption provenance does not exactly match the producing attempt"
                .to_string(),
        );
    }
    let expected_url = candidate_artifact_url(expected_run, &running.task_id, &artifact.id);
    if artifact.url.as_deref() != Some(expected_url.as_str()) {
        return Err(
            "candidate adoption artifact must use the run artifacts evidence URL".to_string(),
        );
    }
    let path = artifact
        .path
        .as_deref()
        .ok_or_else(|| "candidate adoption artifact content is unavailable".to_string())?;
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("read candidate adoption artifact: {error}"))?;
    let sha256 = sha256(&content);
    if artifact.sha256.as_deref() != Some(sha256.as_str()) {
        return Err("candidate adoption SHA-256 does not match artifact content".to_string());
    }
    validate_artifact_content(artifact, &content).map_err(|error| error.message)?;
    Ok(AgentTaskCandidateAdoption {
        source_run_id: expected_run.to_string(),
        source_task_id: running.task_id.clone(),
        source_attempt: running.attempt,
        provider_backend: running.request.executor.backend.clone(),
        provider_selector: running.request.executor.selector.clone(),
        provider_model: running.request.executor.model().map(str::to_string),
        task_base_sha: expected_base.to_string(),
        repository_identity: repository_identity.to_string(),
        workspace_identity: workspace_identity.to_string(),
        artifact_id: artifact.id.clone(),
        sha256,
        content: Some(content),
        ..template.clone()
    })
}

pub(super) fn validate_and_apply_candidate_adoption(
    request: &AgentTaskRequest,
    adoption: &AgentTaskCandidateAdoption,
    task_base_sha: Option<&str>,
) -> Result<(), AgentTaskOutcome> {
    let fail = |message| candidate_adoption_failure(message);
    if adoption.decision != AgentTaskCandidateAdoptionDecision::AdoptPreviousCandidate
        || adoption.task_base_sha != task_base_sha.unwrap_or_default()
        || canonical_repository_identity_for_root(request.workspace.root.as_deref()).as_deref()
            != Some(adoption.repository_identity.as_str())
    {
        return Err(fail(
            "candidate adoption identity or base did not verify".to_string(),
        ));
    }
    let root = request.workspace.root.as_deref().ok_or_else(|| {
        fail("candidate adoption requires an isolated attempt workspace".to_string())
    })?;
    let content = adoption.content.as_deref().ok_or_else(|| {
        fail("candidate adoption content was not resolved from its artifact".to_string())
    })?;
    let normalized =
        normalize_promotion_patch(content, root).map_err(|error| fail(error.message))?;
    if normalized.content != content {
        return Err(fail(
            "candidate adoption payload must already be promotion-normalized".to_string(),
        ));
    }
    for check in [true, false] {
        let mut command = Command::new("git");
        command
            .args(["apply", "--whitespace=nowarn"])
            .arg(if check { "--check" } else { "--index" })
            .arg("-")
            .current_dir(root)
            .stdin(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|error| fail(error.to_string()))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(content.as_bytes())
            .map_err(|error| fail(error.to_string()))?;
        let output = child
            .wait_with_output()
            .map_err(|error| fail(error.to_string()))?;
        if !output.status.success() {
            return Err(fail(format!(
                "candidate adoption patch does not apply: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    Ok(())
}

pub(super) fn attach_candidate_adoption_provenance(
    outcome: &mut AgentTaskOutcome,
    adoption: Option<&AgentTaskCandidateAdoption>,
) {
    let Some(adoption) = adoption else {
        return;
    };
    if !outcome.metadata.is_object() {
        outcome.metadata = serde_json::json!({});
    }
    outcome.metadata["candidate_adoption"] = serde_json::json!({
        "source_run_id": adoption.source_run_id,
        "source_task_id": adoption.source_task_id,
        "source_attempt": adoption.source_attempt,
        "provider_backend": adoption.provider_backend,
        "provider_selector": adoption.provider_selector,
        "provider_model": adoption.provider_model,
        "task_base_sha": adoption.task_base_sha,
        "repository_identity": adoption.repository_identity,
        "workspace_identity": adoption.workspace_identity,
        "artifact_id": adoption.artifact_id,
        "sha256": adoption.sha256,
        "decision": adoption.decision,
    });
}

pub(super) fn finalize_candidate_artifacts(outcome: &mut AgentTaskOutcome, running: &RunningTask) {
    let Some(run_id) = running.run_id.as_deref() else {
        return;
    };
    let repository_identity =
        canonical_repository_identity_for_root(running.source_workspace_root.as_deref());
    let workspace_identity = running
        .source_provenance
        .as_ref()
        .and_then(|value| value.get("workspace_snapshot_identity"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| repository_identity.clone());
    for artifact in &mut outcome.artifacts {
        if !is_actionable_patch_artifact(artifact) {
            continue;
        }
        let Some(path) = artifact.path.as_deref() else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if !artifact.metadata.is_object() {
            artifact.metadata = serde_json::json!({});
        }
        artifact.sha256 = Some(sha256(&content));
        artifact
            .metadata
            .as_object_mut()
            .expect("object metadata")
            .extend(serde_json::Map::from_iter([
                ("run_id".to_string(), serde_json::json!(run_id)),
                ("task_id".to_string(), serde_json::json!(running.task_id)),
                (
                    "producer_attempt".to_string(),
                    serde_json::json!(running.attempt),
                ),
                (
                    "base_ref".to_string(),
                    serde_json::json!(running.task_base_sha),
                ),
                (
                    "provider_backend".to_string(),
                    serde_json::json!(running.request.executor.backend),
                ),
                (
                    "provider_selector".to_string(),
                    serde_json::json!(running.request.executor.selector),
                ),
                (
                    "provider_model".to_string(),
                    serde_json::json!(running.request.executor.model()),
                ),
                (
                    "repository_identity".to_string(),
                    serde_json::json!(repository_identity),
                ),
                (
                    "workspace_identity".to_string(),
                    serde_json::json!(workspace_identity),
                ),
            ]));
        artifact.url = Some(candidate_artifact_url(
            run_id,
            &running.task_id,
            &artifact.id,
        ));
    }
}

fn candidate_adoption_failure(message: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: String::new(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some(message),
        failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata: serde_json::Value::Null,
    }
}

fn candidate_artifact_url(run_id: &str, task_id: &str, artifact_id: &str) -> String {
    use crate::core::execution_contract::encode_uri_component;
    format!(
        "homeboy://agent-task/run/{}/artifacts#task={}&artifact={}",
        encode_uri_component(run_id),
        encode_uri_component(task_id),
        encode_uri_component(artifact_id),
    )
}

fn sha256(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn canonical_repository_identity_for_root(root: Option<&str>) -> Option<String> {
    let remote = crate::core::git::remote_origin_url(Path::new(root?))?;
    let repository = crate::core::deploy::release_download::parse_github_url(&remote)?;
    Some(format!(
        "github://{}/{}/{}",
        repository.host.to_ascii_lowercase(),
        repository.owner.to_ascii_lowercase(),
        repository.repo.to_ascii_lowercase(),
    ))
}
