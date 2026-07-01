use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::agent_task_pr_body::render_pr_body;
use crate::core::error::{Error, Result};
use crate::core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
use crate::core::git::{
    commit_at, get_uncommitted_changes, pr_create, pr_edit, pr_find, push_at, CommitOptions,
    PrCreateOptions, PrEditOptions, PrFindOptions, PrState, PushOptions,
};
use crate::core::proof::{
    HomeboyProof, HomeboyProofArtifactRef, HomeboyProofEnvironmentDisposition,
    HomeboyProofEnvironmentVariable, HomeboyProofProvenance,
};
use crate::core::run_lifecycle_record::RunLifecycleRecord;

pub const AGENT_TASK_PR_FINALIZATION_SCHEMA: &str = "homeboy/agent-task-pr-finalization/v1";
pub const AGENT_TASK_PUBLICATION_INTENT_SCHEMA: &str = "homeboy/agent-task-publication-intent/v1";
pub const AGENT_TASK_PUBLICATION_PROOF_SCHEMA: &str = "homeboy/agent-task-publication-proof/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateResult {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskPrFinalizationReport {
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub path: String,
    pub base: String,
    pub head: String,
    pub pr_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub changed_files: Vec<String>,
    pub gate_results: Vec<AgentTaskGateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalized_gate_results: Vec<HomeboyGateResult>,
    pub proof: HomeboyProof,
    pub publication_intent: AgentTaskPublicationIntent,
    pub publication_proof: AgentTaskPublicationProof,
    #[serde(flatten)]
    pub evidence: AgentTaskPrEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPublicationIntent {
    #[serde(default = "publication_intent_schema")]
    pub schema: String,
    pub run_id: String,
    pub action: String,
    pub target: AgentTaskPublicationTarget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    pub proof: HomeboyProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPublicationTarget {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPublicationProof {
    #[serde(default = "publication_proof_schema")]
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub intent_schema: String,
    pub target: AgentTaskPublicationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_ref: Option<String>,
    pub proof: HomeboyProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrEvidence {
    pub source_refs: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub attempt_summary: String,
    pub ai_tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskPrSourceRelationship::is_empty"
    )]
    pub source_relationship: AgentTaskPrSourceRelationship,
    #[serde(default, skip_serializing_if = "AgentTaskPrVerification::is_empty")]
    pub verification: AgentTaskPrVerification,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskPrRuntimeGuardrails::is_empty"
    )]
    pub runtime_guardrails: AgentTaskPrRuntimeGuardrails,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunLifecycleRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrSourceRelationship {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_finding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_packet_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

impl AgentTaskPrSourceRelationship {
    pub fn is_empty(&self) -> bool {
        self.related_finding_id.is_none()
            && self.source_packet_id.is_none()
            && self.change_kind.is_none()
            && self.supersedes.is_empty()
            && self.depends_on.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrVerification {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targeted_checks_run: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targeted_checks_unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ci_expected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_reviewer_check: Option<String>,
}

impl AgentTaskPrVerification {
    pub fn is_empty(&self) -> bool {
        self.targeted_checks_run.is_empty()
            && self.targeted_checks_unavailable.is_none()
            && self.ci_expected.is_empty()
            && self.manual_reviewer_check.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrRuntimeGuardrails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not_broader_than_packet: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_discriminators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nearby_contracts_preserved: Vec<String>,
}

impl AgentTaskPrRuntimeGuardrails {
    pub fn is_empty(&self) -> bool {
        self.why_not_broader_than_packet.is_none()
            && self.evidence_discriminators.is_empty()
            && self.nearby_contracts_preserved.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct AgentTaskPrFinalizationOptions {
    pub path: String,
    pub run_id: String,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub gate_results: Vec<AgentTaskGateResult>,
    pub normalized_gate_results: Vec<HomeboyGateResult>,
    pub changed_files: Vec<String>,
    pub evidence: AgentTaskPrEvidence,
    pub ai_used_for: String,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskPrRef {
    pub number: u64,
    pub url: String,
}

pub trait AgentTaskPrFinalizationBackend {
    fn current_branch(&mut self, path: &str) -> Result<String>;
    fn changed_files(&mut self, path: &str) -> Result<Vec<String>>;
    fn commit_all(&mut self, path: &str, message: &str) -> Result<()>;
    fn push_branch(&mut self, path: &str, head: &str) -> Result<()>;
    fn find_open_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>>;
    fn create_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
    fn update_pr(
        &mut self,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
}

pub struct RealAgentTaskPrFinalizationBackend;

impl AgentTaskPrFinalizationBackend for RealAgentTaskPrFinalizationBackend {
    fn current_branch(&mut self, path: &str) -> Result<String> {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn changed_files(&mut self, path: &str) -> Result<Vec<String>> {
        let changes = get_uncommitted_changes(path)?;
        let mut files = changes.staged;
        files.extend(changes.unstaged);
        files.extend(changes.untracked);
        files.sort();
        files.dedup();
        Ok(files)
    }

    fn commit_all(&mut self, path: &str, message: &str) -> Result<()> {
        let output = commit_at(None, Some(message), CommitOptions::default(), Some(path))?;
        if !output.success {
            return Err(Error::git_command_failed(format!(
                "git commit failed: {}",
                output.stderr
            )));
        }
        Ok(())
    }

    fn push_branch(&mut self, path: &str, head: &str) -> Result<()> {
        let output = push_at(
            None,
            PushOptions {
                refspec: Some(format!("HEAD:refs/heads/{}", head)),
                ..Default::default()
            },
            Some(path),
        )?;
        if !output.success {
            return Err(Error::git_command_failed(format!(
                "git push failed: {}",
                output.stderr
            )));
        }
        Ok(())
    }

    fn find_open_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        let output = pr_find(
            None,
            PrFindOptions {
                base: Some(base.to_string()),
                head: Some(head.to_string()),
                state: PrState::Open,
                limit: 10,
                path: Some(path.to_string()),
            },
        )?;
        Ok(output.items.into_iter().next().map(|item| AgentTaskPrRef {
            number: item.number,
            url: item.url,
        }))
    }

    fn create_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        let output = pr_create(
            None,
            PrCreateOptions {
                base: base.to_string(),
                head: head.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                draft: false,
                path: Some(path.to_string()),
            },
        )?;
        Ok(AgentTaskPrRef {
            number: output.number.unwrap_or_default(),
            url: output.url.unwrap_or_default(),
        })
    }

    fn update_pr(
        &mut self,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        let output = pr_edit(
            None,
            PrEditOptions {
                number,
                title: Some(title.to_string()),
                body: Some(body.to_string()),
                path: Some(path.to_string()),
            },
        )?;
        Ok(AgentTaskPrRef {
            number,
            url: output.url.unwrap_or_default(),
        })
    }
}

pub fn finalize_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend(options, &mut RealAgentTaskPrFinalizationBackend)
}

pub fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    validate_green_gates(&options.normalized_gate_results)?;
    let proof = build_finalization_proof(&options, options.normalized_gate_results.clone());
    let head = options
        .head
        .clone()
        .map(Ok)
        .unwrap_or_else(|| backend.current_branch(&options.path))?;
    refuse_protected_head(&head, &options.protected_branches)?;

    let mut changed_files = if options.changed_files.is_empty() {
        backend.changed_files(&options.path)?
    } else {
        options.changed_files.clone()
    };
    changed_files.sort();
    changed_files.dedup();
    let intent = build_pr_publication_intent(&options, &head, &changed_files, proof.clone());
    validate_publication_intent(&intent)?;

    if changed_files.is_empty() {
        return Ok(report(
            &options,
            intent,
            &head,
            "no_changes",
            "none",
            None,
            None,
            changed_files,
            Some(proof),
        ));
    }

    backend.commit_all(&options.path, &options.commit_message)?;
    backend.push_branch(&options.path, &head)?;
    let body = render_pr_body(&options, &proof, &head, &changed_files);
    let existing = backend.find_open_pr(&options.path, &options.base, &head)?;
    let (action, pr) = match existing {
        Some(existing) => (
            "updated",
            backend.update_pr(&options.path, existing.number, &options.title, &body)?,
        ),
        None => (
            "created",
            backend.create_pr(&options.path, &options.base, &head, &options.title, &body)?,
        ),
    };

    Ok(report(
        &options,
        intent,
        &head,
        "review_ready",
        action,
        Some(pr.number),
        Some(pr.url),
        changed_files,
        Some(proof),
    ))
}

fn validate_green_gates(gates: &[HomeboyGateResult]) -> Result<()> {
    if gates.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            "at least one deterministic green gate is required before PR finalization",
            None,
            None,
        ));
    }
    let red: Vec<String> = gates
        .iter()
        .filter(|gate| gate.status != HomeboyGateStatus::Passed)
        .map(|gate| format!("{}={:?}", gate.name, gate.status))
        .collect();
    if !red.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            format!(
                "finalization requires green gates; red gates: {}",
                red.join(", ")
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn is_green_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "green" | "passed" | "pass" | "succeeded" | "success" | "ok"
    )
}

impl From<AgentTaskGateResult> for HomeboyGateResult {
    fn from(gate: AgentTaskGateResult) -> Self {
        gate_result_from_legacy(gate)
    }
}

pub(crate) fn gate_result_from_legacy(gate: AgentTaskGateResult) -> HomeboyGateResult {
    let status = if is_green_status(&gate.status) {
        HomeboyGateStatus::Passed
    } else {
        HomeboyGateStatus::Failed
    };
    let summary = match gate
        .detail
        .as_deref()
        .filter(|detail| !detail.trim().is_empty())
    {
        Some(detail) => format!("{}: {} ({detail})", gate.name, gate.status),
        None => format!("{}: {}", gate.name, gate.status),
    };

    HomeboyGateResult::new(
        format!("finalization.gate.{}", gate.name),
        gate.name.clone(),
        HomeboyGateKind::Command,
        status,
    )
    .summary(summary)
    .evidence(json!({
        "name": gate.name,
        "status": gate.status,
        "detail": gate.detail,
    }))
    .retryable(status == HomeboyGateStatus::Failed)
    .provenance(json!({
        "source_type": "AgentTaskGateResult",
    }))
}

fn refuse_protected_head(head: &str, protected_branches: &[String]) -> Result<()> {
    if protected_branches.iter().any(|branch| branch == head) {
        return Err(Error::validation_invalid_argument(
            "head",
            format!(
                "refusing to finalize directly on protected branch '{}'",
                head
            ),
            None,
            Some(protected_branches.to_vec()),
        ));
    }
    Ok(())
}

pub fn validate_publication_intent(intent: &AgentTaskPublicationIntent) -> Result<()> {
    if intent.schema != AGENT_TASK_PUBLICATION_INTENT_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "publication_intent.schema",
            "publication intent schema is not supported",
            None,
            Some(vec![intent.schema.clone()]),
        ));
    }
    if intent.run_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.run_id",
            "publication intent requires a run id",
            None,
            None,
        ));
    }
    if intent.action.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.action",
            "publication intent requires an action",
            None,
            None,
        ));
    }
    if intent.target.kind.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.kind",
            "publication intent requires a target kind",
            None,
            None,
        ));
    }
    if intent
        .target
        .head
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.head",
            "publication intent requires a target head ref",
            None,
            None,
        ));
    }
    Ok(())
}

fn build_pr_publication_intent(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
    proof: HomeboyProof,
) -> AgentTaskPublicationIntent {
    AgentTaskPublicationIntent {
        schema: AGENT_TASK_PUBLICATION_INTENT_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        action: "review_request".to_string(),
        target: AgentTaskPublicationTarget {
            kind: "code_review".to_string(),
            adapter: Some("github_pull_request".to_string()),
            path: Some(options.path.clone()),
            base: Some(options.base.clone()),
            head: Some(head.to_string()),
            url: None,
        },
        changed_files: changed_files.to_vec(),
        source_refs: options.evidence.source_refs.clone(),
        artifact_refs: options.evidence.artifact_refs.clone(),
        proof,
    }
}

fn publication_proof(
    intent: &AgentTaskPublicationIntent,
    status: &str,
    adapter_action: &str,
    adapter_ref: Option<String>,
) -> AgentTaskPublicationProof {
    let mut target = intent.target.clone();
    target.url = adapter_ref.clone();
    AgentTaskPublicationProof {
        schema: AGENT_TASK_PUBLICATION_PROOF_SCHEMA.to_string(),
        run_id: intent.run_id.clone(),
        status: status.to_string(),
        intent_schema: intent.schema.clone(),
        target,
        adapter_action: (adapter_action != "none").then(|| adapter_action.to_string()),
        adapter_ref,
        proof: intent.proof.clone(),
    }
}

fn report(
    options: &AgentTaskPrFinalizationOptions,
    mut publication_intent: AgentTaskPublicationIntent,
    head: &str,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: Vec<String>,
    proof: Option<HomeboyProof>,
) -> AgentTaskPrFinalizationReport {
    let normalized_gate_results = options.normalized_gate_results.clone();
    let proof =
        proof.unwrap_or_else(|| build_finalization_proof(options, normalized_gate_results.clone()));
    publication_intent.proof = proof.clone();
    let publication_proof =
        publication_proof(&publication_intent, status, pr_action, pr_url.clone());
    AgentTaskPrFinalizationReport {
        schema: AGENT_TASK_PR_FINALIZATION_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        status: status.to_string(),
        path: options.path.clone(),
        base: options.base.clone(),
        head: head.to_string(),
        pr_action: pr_action.to_string(),
        pr_number,
        pr_url,
        changed_files,
        gate_results: options.gate_results.clone(),
        normalized_gate_results,
        proof,
        publication_intent,
        publication_proof,
        evidence: options.evidence.clone(),
    }
}

fn publication_intent_schema() -> String {
    AGENT_TASK_PUBLICATION_INTENT_SCHEMA.to_string()
}

fn publication_proof_schema() -> String {
    AGENT_TASK_PUBLICATION_PROOF_SCHEMA.to_string()
}

fn build_finalization_proof(
    options: &AgentTaskPrFinalizationOptions,
    gates: Vec<HomeboyGateResult>,
) -> HomeboyProof {
    let provenance = HomeboyProofProvenance::homeboy_run(options.run_id.clone())
        .source_refs(options.evidence.source_refs.clone());
    let artifacts = options
        .evidence
        .artifact_refs
        .iter()
        .cloned()
        .map(HomeboyProofArtifactRef::uri);
    let environment = proof_environment_from_gates(&gates);

    HomeboyProof::new(
        format!("agent-task-finalization:{}", options.run_id),
        provenance,
    )
    .gates_requiring_ci_equivalent(gates)
    .artifacts(artifacts)
    .environment(environment)
}

fn proof_environment_from_gates(
    gates: &[HomeboyGateResult],
) -> Vec<HomeboyProofEnvironmentVariable> {
    let mut environment = Vec::new();
    for gate in gates {
        let Some(gate_environment) = gate.evidence.get("environment") else {
            continue;
        };
        environment.extend(proof_environment_variables(
            gate_environment.get("inherited"),
            HomeboyProofEnvironmentDisposition::Inherited,
        ));
        environment.extend(proof_environment_variables(
            gate_environment.get("sanitized"),
            HomeboyProofEnvironmentDisposition::Sanitized,
        ));
    }
    environment.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.value.cmp(&right.value))
            .then(format!("{:?}", left.disposition).cmp(&format!("{:?}", right.disposition)))
    });
    environment.dedup();
    environment
}

fn proof_environment_variables(
    variables: Option<&serde_json::Value>,
    disposition: HomeboyProofEnvironmentDisposition,
) -> Vec<HomeboyProofEnvironmentVariable> {
    variables
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|variable| {
            let name = variable.get("name")?.as_str()?;
            let value = variable.get("value")?.as_str()?;
            Some(HomeboyProofEnvironmentVariable {
                name: name.to_string(),
                value: value.to_string(),
                disposition,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::run_lifecycle_record::{
        ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
        ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
        ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
    };

    #[derive(Default)]
    struct MockBackend {
        branch: String,
        changed_files: Vec<String>,
        existing_pr: Option<AgentTaskPrRef>,
        create_error: bool,
        committed: bool,
        pushed: bool,
        created: bool,
        updated: bool,
        last_body: String,
    }

    impl AgentTaskPrFinalizationBackend for MockBackend {
        fn current_branch(&mut self, _path: &str) -> Result<String> {
            Ok(if self.branch.is_empty() {
                "fix/cook".to_string()
            } else {
                self.branch.clone()
            })
        }

        fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
            Ok(self.changed_files.clone())
        }

        fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
            self.committed = true;
            Ok(())
        }

        fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
            self.pushed = true;
            Ok(())
        }

        fn find_open_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<Option<AgentTaskPrRef>> {
            Ok(self.existing_pr.clone())
        }

        fn create_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            if self.create_error {
                return Err(Error::git_command_failed("gh pr create failed"));
            }
            self.created = true;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number: 123,
                url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            })
        }

        fn update_pr(
            &mut self,
            _path: &str,
            number: u64,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.updated = true;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number,
                url: format!("https://github.com/Extra-Chill/homeboy/pull/{}", number),
            })
        }
    }

    #[test]
    fn creates_new_pr_after_green_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "created");
        assert_eq!(report.pr_number, Some(123));
        assert!(backend.committed);
        assert!(backend.pushed);
        assert!(backend.created);
        assert!(backend.last_body.contains("## AI assistance"));
        assert!(backend.last_body.contains("## Proof provenance"));
        assert!(backend.last_body.contains("## Publication intent"));
        assert!(backend
            .last_body
            .contains("**Adapter:** `github_pull_request`"));
        assert!(backend
            .last_body
            .contains("**Proof runner:** Homeboy agent-task cook loop"));
        assert!(backend
            .last_body
            .contains("**Homeboy run ID:** `cook-3678`"));
        assert!(backend.last_body.contains("## Gate results"));
        assert!(backend.last_body.contains("## CI-equivalent coverage"));
        assert!(backend
            .last_body
            .contains("CI-equivalent required gate was not recorded"));
        assert!(backend.last_body.contains("review-ready"));
        assert_eq!(
            report.publication_intent.schema,
            AGENT_TASK_PUBLICATION_INTENT_SCHEMA
        );
        assert_eq!(report.publication_intent.action, "review_request");
        assert_eq!(report.publication_intent.target.kind, "code_review");
        assert_eq!(
            report.publication_intent.target.adapter.as_deref(),
            Some("github_pull_request")
        );
        assert_eq!(
            report.publication_proof.schema,
            AGENT_TASK_PUBLICATION_PROOF_SCHEMA
        );
        assert_eq!(
            report.publication_proof.adapter_action.as_deref(),
            Some("created")
        );
        assert_eq!(
            report.publication_proof.adapter_ref.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/pull/123")
        );
    }

    #[test]
    fn pr_body_labels_ci_equivalent_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.normalized_gate_results = vec![HomeboyGateResult::new(
            "gate-1",
            "required project gate",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .evidence(json!({
            "command": ["sh", "-lc", "project verify"],
            "exit_code": 0,
            "ci_equivalent": true,
        }))];

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(backend
            .last_body
            .contains("required project gate: passed (CI-equivalent)"));
        assert!(backend.last_body.contains("required project gate: passed"));
        assert!(!backend.last_body.contains("not recorded by Homeboy"));
    }

    #[test]
    fn pr_body_reports_iterator_evidence_metadata() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.evidence.source_relationship = AgentTaskPrSourceRelationship {
            related_finding_id: Some("finding-123".to_string()),
            source_packet_id: Some("packet-456".to_string()),
            change_kind: Some("runtime-fix".to_string()),
            supersedes: vec!["https://github.com/org/repo/pull/1".to_string()],
            depends_on: vec!["https://github.com/org/repo/pull/2".to_string()],
        };
        options.evidence.verification = AgentTaskPrVerification {
            targeted_checks_run: vec!["cargo test pr_body".to_string()],
            targeted_checks_unavailable: None,
            ci_expected: vec!["Homeboy CI".to_string()],
            manual_reviewer_check: None,
        };
        options.evidence.runtime_guardrails = AgentTaskPrRuntimeGuardrails {
            why_not_broader_than_packet: Some("Preserves class and href gates.".to_string()),
            evidence_discriminators: vec!["class=brand".to_string(), "href=#top".to_string()],
            nearby_contracts_preserved: vec!["is_branded_inline_anchor".to_string()],
        };

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(backend.last_body.contains("## Source relationship"));
        assert!(backend.last_body.contains("finding-123"));
        assert!(backend.last_body.contains("## Verification capability"));
        assert!(backend
            .last_body
            .contains("`targeted_checks_run`: `cargo test pr_body`"));
        assert!(backend.last_body.contains("## Runtime guardrails"));
        assert!(backend
            .last_body
            .contains("Preserves class and href gates."));
        assert!(backend.last_body.contains("## Sibling PR relationship"));
        assert!(backend.last_body.contains("- **Model:** GPT-5.5"));
    }

    #[test]
    fn pr_body_reports_run_lifecycle_evidence() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.evidence.lifecycle = Some(RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Succeeded,
                started_at: None,
                finished_at: Some("2026-06-16T00:00:05Z".to_string()),
                updated_at: Some("2026-06-16T00:00:05Z".to_string()),
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                state: ProviderRuntimeState::Succeeded,
                stream_uri: None,
                external_runtime_ids: vec![ExternalRuntimeId {
                    kind: "provider_run_id".to_string(),
                    value: "provider-run-123".to_string(),
                    provider: Some("sample-runtime".to_string()),
                    url: None,
                }],
                metadata: serde_json::Value::Null,
            }],
            external_runtime_ids: vec![ExternalRuntimeId {
                kind: "provider_run_id".to_string(),
                value: "provider-run-123".to_string(),
                provider: Some("sample-runtime".to_string()),
                url: None,
            }],
            cleanup: CleanupLifecycle {
                state: CleanupState::Preserved,
                policy: Some("preserve".to_string()),
                updated_at: None,
            },
            finalization: FinalizationLifecycle {
                state: FinalizationState::Pending,
                updated_at: None,
            },
            artifact_retention: ArtifactRetentionLifecycle {
                status: ArtifactRetentionStatus::Retained,
                policy: Some("retain".to_string()),
                updated_at: None,
            },
            ..RunLifecycleRecord::default()
        });

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(backend.last_body.contains("## Run lifecycle"));
        assert!(backend
            .last_body
            .contains("provider_run_id:provider-run-123"));
        assert!(backend
            .last_body
            .contains("Artifact retention:** `Retained`"));
    }

    #[test]
    fn updates_existing_pr_for_same_branch() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            existing_pr: Some(AgentTaskPrRef {
                number: 77,
                url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            }),
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "updated");
        assert_eq!(report.pr_number, Some(77));
        assert!(backend.updated);
        assert!(!backend.created);
    }

    #[test]
    fn reports_no_changes_without_commit_push_or_pr() {
        let mut backend = MockBackend::default();

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "no_changes");
        assert_eq!(report.pr_action, "none");
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    }

    #[test]
    fn refuses_protected_branch() {
        let mut backend = MockBackend {
            branch: "main".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("blocked");

        assert!(error.message.contains("protected branch"));
        assert!(!backend.committed);
    }

    #[test]
    fn propagates_pr_creation_failure() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            create_error: true,
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("failed");

        assert!(error.message.contains("gh pr create failed"));
        assert!(backend.committed);
        assert!(backend.pushed);
    }

    #[test]
    fn refuses_red_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.gate_results[0].status = "failed".to_string();
        options.normalized_gate_results[0] =
            HomeboyGateResult::from(options.gate_results[0].clone());

        let error = finalize_pr_with_backend(options, &mut backend).expect_err("blocked");

        assert!(error.message.contains("green gates"));
        assert!(!backend.committed);
    }

    #[test]
    fn validates_publication_intent_contract() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        validate_publication_intent(&report.publication_intent).expect("intent is valid");

        let mut invalid = report.publication_intent;
        invalid.target.head = Some(String::new());
        let error = validate_publication_intent(&invalid).expect_err("missing head rejected");

        assert!(error.message.contains("target head ref"));
    }

    fn options() -> AgentTaskPrFinalizationOptions {
        let gate_results = vec![AgentTaskGateResult {
            name: "focused project check".to_string(),
            status: "passed".to_string(),
            detail: Some("targeted".to_string()),
        }];
        let normalized_gate_results = gate_results
            .iter()
            .cloned()
            .map(HomeboyGateResult::from)
            .collect();

        AgentTaskPrFinalizationOptions {
            path: "/repo".to_string(),
            run_id: "cook-3678".to_string(),
            base: "main".to_string(),
            head: None,
            title: "Cook issue #3678".to_string(),
            commit_message: "finalize cook loop PR plumbing".to_string(),
            gate_results,
            normalized_gate_results,
            changed_files: Vec::new(),
            evidence: AgentTaskPrEvidence {
                source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/3678".to_string()],
                artifact_refs: vec!["artifact://aggregate.json".to_string()],
                attempt_summary: "attempt 1 passed deterministic gates".to_string(),
                ai_tool: "OpenCode (GPT-5.5)".to_string(),
                ai_model: Some("GPT-5.5".to_string()),
                source_relationship: AgentTaskPrSourceRelationship::default(),
                verification: AgentTaskPrVerification::default(),
                runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
                lifecycle: None,
            },
            ai_used_for: "Drafted implementation and tests; Chris reviews and owns the change."
                .to_string(),
            protected_branches: vec![
                "main".to_string(),
                "master".to_string(),
                "trunk".to_string(),
            ],
        }
    }
}
