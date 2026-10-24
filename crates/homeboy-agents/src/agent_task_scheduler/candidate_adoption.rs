use std::path::Path;

use super::*;

pub(super) fn select_candidate_adoption(
    template: &AgentTaskCandidateAdoption,
    candidate: Option<&crate::agent_task::AgentTaskCandidate>,
    running: &RunningTask,
) -> Result<AgentTaskCandidateAdoption, String> {
    let candidate = candidate
        .ok_or_else(|| "non-recoverable: attempt has no immutable candidate".to_string())?;
    Ok(AgentTaskCandidateAdoption {
        source_run_id: running.run_id.clone().unwrap_or_default(),
        source_task_id: running.task_id.clone(),
        source_attempt: running.attempt,
        provider_backend: running.request.executor.backend.clone(),
        provider_selector: running.request.executor.selector.clone(),
        provider_model: running.request.executor.model().map(str::to_string),
        task_base_sha: candidate.base_sha.clone(),
        candidate: Some(candidate.clone()),
        ..template.clone()
    })
}

#[expect(clippy::result_large_err, reason = "durable scheduler outcome")]
pub(super) fn validate_and_apply_candidate_adoption(
    request: &AgentTaskRequest,
    adoption: &AgentTaskCandidateAdoption,
    task_base_sha: Option<&str>,
) -> Result<(), AgentTaskOutcome> {
    let candidate = adoption.candidate.as_ref().ok_or_else(|| {
        candidate_adoption_failure(
            "non-recoverable: adoption has no immutable candidate".to_string(),
        )
    })?;
    let root = request.workspace.root.as_deref().ok_or_else(|| {
        candidate_adoption_failure(
            "candidate adoption requires an isolated attempt workspace".to_string(),
        )
    })?;
    if task_base_sha != Some(candidate.base_sha.as_str()) {
        return Err(candidate_adoption_failure(
            "candidate adoption base did not verify".to_string(),
        ));
    }
    crate::agent_task_promotion::validate_candidate_commit(Path::new(root), candidate)
        .map_err(|error| candidate_adoption_failure(error.message))?;
    super::git_output(
        Path::new(root),
        &["checkout", "--detach", &candidate.candidate_sha],
    )
    .map_err(|error| candidate_adoption_failure(format!("{error:?}")))?;
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
        "candidate": adoption.candidate,
        "decision": adoption.decision,
    });
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
        candidate: None,
    }
}
