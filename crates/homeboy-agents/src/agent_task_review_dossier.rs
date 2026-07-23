use regex::Regex;
use serde::{Deserialize, Serialize};

use homeboy_core::error::{Error, Result};
use homeboy_core::gate::HomeboyGateResult;

pub const AGENT_TASK_REVIEW_DOSSIER_SCHEMA: &str = "homeboy/agent-task-review-dossier/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskReviewSectionId {
    Summary,
    WhatChanged,
    HowToTest,
    Compatibility,
    Evidence,
    AiAssistance,
    SourceRelationships,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentTaskReviewProfile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_sections: Vec<AgentTaskReviewSectionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_sections: Vec<AgentTaskReviewSectionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub section_order: Vec<AgentTaskReviewSectionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_sections: Vec<AgentTaskReviewAdditionalSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentTaskReviewAdditionalSection {
    pub id: String,
    pub heading: String,
    pub content: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewTestStep {
    pub command: String,
    pub expected: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewEvidence {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPublicContract {
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskExternalUsageStatus {
    Completed,
    UnavailableManualReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExternalUsageEvidence {
    pub status: AgentTaskExternalUsageStatus,
    pub source: String,
    pub limitations: String,
    pub url: String,
}

/// Reviewer-facing evidence required when this change declares a public contract.
/// The contract itself remains generic; callers supply their own ecosystem meaning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPublicContractEvidence {
    pub compatibility_impact: String,
    pub external_consumer_impact: String,
    pub external_usage: AgentTaskExternalUsageEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewDossier {
    #[serde(default = "dossier_schema")]
    pub schema: String,
    pub summary: String,
    pub what_changed: Vec<String>,
    pub how_to_test: Vec<AgentTaskReviewTestStep>,
    pub compatibility: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AgentTaskReviewEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_public_contracts: Vec<AgentTaskPublicContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_contract_evidence: Option<AgentTaskPublicContractEvidence>,
    pub ai_assistance: AgentTaskReviewAiAssistance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_relationships: Vec<AgentTaskReviewIssueRelationship>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<AgentTaskReviewOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewAiAssistance {
    pub used: bool,
    pub tool: String,
    pub model: String,
    pub used_for: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskReviewIssueRelationshipKind {
    Closes,
    RelatesTo,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewIssueRelationship {
    pub kind: AgentTaskReviewIssueRelationshipKind,
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskReviewOverrideTarget {
    Summary,
    WhatChanged,
    Compatibility,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskReviewOverride {
    pub target: AgentTaskReviewOverrideTarget,
    pub value: String,
    pub provenance: String,
}

/// Schema key the agent emits its review form under inside `AgentTaskOutcome.outputs`.
pub const AI_REVIEW_FORM_OUTPUT_KEY: &str = "review_form";

/// The single AI-authored "form" — every non-deterministic slot of the review
/// dossier. The orchestrator owns everything else (AI-assistance tool/model,
/// evidence, gate labels, how-to-test, source relationships, the section
/// skeleton). The agent fills this once and returns it via
/// `AgentTaskOutcome.outputs["review_form"]`; it is the only reviewer-facing
/// prose the model authors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AiFilledReviewForm {
    /// What is changing in this PR and why.
    pub summary: String,
    /// Bullet points of the concrete changes.
    #[serde(default)]
    pub what_changed: Vec<String>,
    /// Compatibility / impact assessment.
    pub compatibility: String,
    /// Self-reflective, concise description of the *process* the AI took —
    /// deliberately distinct from `summary` (which describes *what* changed).
    pub used_for: String,
}

impl AiFilledReviewForm {
    /// Parse the form the agent emitted under `outputs["review_form"]`.
    ///
    /// Returns `Ok(None)` when the key is absent (the agent did not emit a
    /// form at all — the loop treats that like a red gate and nudges a retry).
    /// A present-but-malformed value is a hard parse error so a garbage form
    /// is never silently rendered.
    pub fn from_outcome_outputs(outputs: &serde_json::Value) -> Result<Option<Self>> {
        let Some(value) = outputs.get(AI_REVIEW_FORM_OUTPUT_KEY) else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let form: Self = serde_json::from_value(value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "review_form",
                format!("agent-emitted review_form is malformed: {error}"),
                None,
                None,
            )
        })?;
        Ok(Some(form))
    }

    /// Reviewer-facing feedback describing exactly what a valid form requires.
    /// Surfaced to the agent when the form is missing or incomplete so the
    /// nudge loop can converge.
    pub fn requirement_feedback() -> &'static str {
        "Emit a `review_form` object in your task outputs with: `summary` (what is changing and why), \
`what_changed` (a non-empty list of concrete change bullets), `compatibility` (impact/compatibility \
assessment), and `used_for` (a concise, self-reflective description of the process you took — distinct \
from the summary of what changed). `used_for` must be a genuine reflection, not a restatement of the summary."
    }

    /// Validate that the agent filled every required slot with real content.
    /// `Err` carries actionable, agent-facing feedback for the nudge loop.
    pub fn validate(&self) -> Result<()> {
        if self.summary.trim().is_empty() {
            return Err(review_form_gap("review_form.summary", "summary is empty"));
        }
        if self
            .what_changed
            .iter()
            .all(|entry| entry.trim().is_empty())
        {
            return Err(review_form_gap(
                "review_form.what_changed",
                "what_changed has no non-empty entries",
            ));
        }
        if self.compatibility.trim().is_empty() {
            return Err(review_form_gap(
                "review_form.compatibility",
                "compatibility is empty",
            ));
        }
        if self.used_for.trim().is_empty() {
            return Err(review_form_gap("review_form.used_for", "used_for is empty"));
        }
        if used_for_is_placeholder(&self.used_for) {
            return Err(review_form_gap(
                "review_form.used_for",
                "used_for is a placeholder, not a genuine process reflection",
            ));
        }
        // A used_for that merely restates the summary is not a reflection.
        if self
            .used_for
            .trim()
            .eq_ignore_ascii_case(self.summary.trim())
        {
            return Err(review_form_gap(
                "review_form.used_for",
                "used_for must be distinct from summary (it describes the process, not the change)",
            ));
        }
        Ok(())
    }
}

fn review_form_gap(field: &str, problem: &str) -> Error {
    Error::validation_invalid_argument(
        field,
        format!("{problem}. {}", AiFilledReviewForm::requirement_feedback()),
        None,
        None,
    )
}

/// Canned/placeholder `used_for` values that must not pass as a genuine
/// reflection — including the legacy CLI default this refactor retires.
fn used_for_is_placeholder(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "n/a"
            | "na"
            | "none"
            | "ai-assisted"
            | "ai assisted"
            | "implementation"
            | "drafted implementation and tests; chris reviews and owns the change."
    )
}

/// Compose the deterministic AI-assistance tool disclosure for a generated PR.
///
/// The tool string is orchestrator-owned (not AI-authored), which makes it the
/// honest place to attribute the orchestrator that actually drove the change.
/// Every PR Homeboy opens names Homeboy as the harness, wrapping the underlying
/// provider disclosure: `Homeboy (OpenCode / openai/gpt-5.6-terra)`.
///
/// If the provider disclosure is already Homeboy-attributed (idempotent) or
/// empty, it is passed through unchanged.
pub fn homeboy_tool_disclosure(provider_disclosure: &str) -> String {
    let provider = provider_disclosure.trim();
    if provider.is_empty() || provider.starts_with("Homeboy") {
        return provider.to_string();
    }
    format!("Homeboy ({provider})")
}

pub fn default_profile() -> AgentTaskReviewProfile {
    AgentTaskReviewProfile {
        required_sections: vec![
            AgentTaskReviewSectionId::Summary,
            AgentTaskReviewSectionId::WhatChanged,
            AgentTaskReviewSectionId::HowToTest,
            AgentTaskReviewSectionId::Compatibility,
            AgentTaskReviewSectionId::AiAssistance,
        ],
        ..Default::default()
    }
}

/// The component's portable config is the only profile source. Invalid portable
/// config therefore fails finalization instead of being mistaken for profile absence.
pub fn resolve_review_profile(path: &str) -> Result<AgentTaskReviewProfile> {
    let component = homeboy_core::component::resolve_effective(None, Some(path), None)?;
    // The component model carries the profile opaquely as JSON; deserialize it
    // here (the agent-task layer owns the profile schema). A present-but-invalid
    // profile fails finalization instead of being mistaken for profile absence.
    let profile = match component.review_profile {
        Some(value) => serde_json::from_value(value).map_err(|error| {
            homeboy_core::Error::validation_invalid_json(
                error,
                Some("parse component review profile".to_string()),
                None,
            )
        })?,
        None => default_profile(),
    };
    validate_profile(&profile)?;
    Ok(profile)
}

pub fn validate_profile(profile: &AgentTaskReviewProfile) -> Result<()> {
    unique_sections("required_sections", &profile.required_sections)?;
    unique_sections("hidden_sections", &profile.hidden_sections)?;
    unique_sections("section_order", &profile.section_order)?;
    if profile
        .required_sections
        .iter()
        .any(|id| profile.hidden_sections.contains(id))
    {
        return invalid(
            "review_profile",
            "required and hidden sections cannot conflict",
        );
    }
    if profile
        .hidden_sections
        .contains(&AgentTaskReviewSectionId::HowToTest)
    {
        return invalid(
            "review_profile",
            "How to test cannot be hidden from generated PRs",
        );
    }
    let mut additional = std::collections::BTreeSet::new();
    for section in &profile.additional_sections {
        scalar("review_profile.additional_sections.id", &section.id)?;
        scalar(
            "review_profile.additional_sections.heading",
            &section.heading,
        )?;
        scalar(
            "review_profile.additional_sections.content",
            &section.content,
        )?;
        if !additional.insert(section.id.clone()) {
            return invalid(
                "review_profile.additional_sections",
                "additional section IDs must be unique",
            );
        }
        if builtin_section_name(&section.id) {
            return invalid(
                "review_profile.additional_sections",
                "additional section IDs cannot collide with built-in sections",
            );
        }
        if section.required && section.content.is_empty() {
            return invalid(
                "review_profile.additional_sections",
                "required additional sections need content",
            );
        }
    }
    Ok(())
}

impl AgentTaskReviewDossier {
    pub fn apply_overrides(&mut self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for override_ in &self.overrides {
            scalar("review_dossier.override.value", &override_.value)?;
            scalar("review_dossier.override.provenance", &override_.provenance)?;
            if !seen.insert(override_.target.clone()) {
                return invalid(
                    "review_dossier.overrides",
                    "each override target may be set once",
                );
            }
            match override_.target {
                AgentTaskReviewOverrideTarget::Summary => self.summary = override_.value.clone(),
                AgentTaskReviewOverrideTarget::WhatChanged => {
                    self.what_changed = vec![override_.value.clone()]
                }
                AgentTaskReviewOverrideTarget::Compatibility => {
                    self.compatibility = override_.value.clone()
                }
            }
        }
        Ok(())
    }

    pub fn validate(&self, profile: &AgentTaskReviewProfile) -> Result<()> {
        validate_profile(profile)?;
        if self.schema != AGENT_TASK_REVIEW_DOSSIER_SCHEMA {
            return invalid("schema", "review dossier schema is not supported");
        }
        scalar("summary", &self.summary)?;
        scalar("compatibility", &self.compatibility)?;
        for value in &self.what_changed {
            scalar("what_changed", value)?;
        }
        scalar("ai_assistance.tool", &self.ai_assistance.tool)?;
        scalar("ai_assistance.model", &self.ai_assistance.model)?;
        scalar("ai_assistance.used_for", &self.ai_assistance.used_for)?;
        for step in &self.how_to_test {
            scalar("how_to_test.command", &step.command)?;
            scalar("how_to_test.expected", &step.expected)?;
            if step.command.trim().is_empty() || step.expected.trim().is_empty() {
                return invalid(
                    "how_to_test",
                    "each test step needs a runnable command and expected result",
                );
            }
            if !reviewer_runnable_command(&step.command) {
                return invalid(
                    "how_to_test.command",
                    "test commands must be reviewer-runnable and cannot contain operator-only references",
                );
            }
        }
        for (index, evidence) in self.evidence.iter().enumerate() {
            scalar("evidence.summary", &evidence.summary)?;
            if let Some(url) = &evidence.url {
                validate_reviewer_url(
                    &format!("evidence[{index}].url"),
                    url,
                    &format!("review dossier evidence summary `{}`", evidence.summary),
                )?;
            }
        }
        for contract in &self.changed_public_contracts {
            scalar("changed_public_contracts.id", &contract.id)?;
            scalar("changed_public_contracts.summary", &contract.summary)?;
            if contract.id.trim().is_empty() || contract.summary.trim().is_empty() {
                return invalid(
                    "changed_public_contracts",
                    "each declared public contract needs an identifier and summary",
                );
            }
        }
        match (&self.changed_public_contracts[..], &self.public_contract_evidence) {
            ([], Some(_)) => {
                return invalid(
                    "public_contract_evidence",
                    "public-contract evidence requires a declared changed public contract",
                )
            }
            (contracts, None) if !contracts.is_empty() => {
                return invalid(
                    "public_contract_evidence",
                    "declared public contracts require compatibility, external-consumer, and external-usage evidence",
                )
            }
            (_, Some(evidence)) => {
                scalar(
                    "public_contract_evidence.compatibility_impact",
                    &evidence.compatibility_impact,
                )?;
                scalar(
                    "public_contract_evidence.external_consumer_impact",
                    &evidence.external_consumer_impact,
                )?;
                scalar("public_contract_evidence.external_usage.source", &evidence.external_usage.source)?;
                scalar("public_contract_evidence.external_usage.limitations", &evidence.external_usage.limitations)?;
                validate_reviewer_url(
                    "public_contract_evidence.external_usage.url",
                    &evidence.external_usage.url,
                    &format!(
                        "public-contract external usage source `{}`",
                        evidence.external_usage.source
                    ),
                )?;
                if evidence.compatibility_impact.trim().is_empty()
                    || evidence.external_consumer_impact.trim().is_empty()
                    || evidence.external_usage.source.trim().is_empty()
                    || evidence.external_usage.limitations.trim().is_empty()
                {
                    return invalid(
                        "public_contract_evidence",
                        "public-contract evidence requires non-empty compatibility impact, external-consumer impact, usage source, and usage limitations",
                    );
                }
            }
            _ => {}
        }
        for relationship in &self.source_relationships {
            validate_issue_reference(&relationship.reference)?;
        }
        if required(profile, AgentTaskReviewSectionId::Summary) && self.summary.is_empty() {
            return invalid("summary", "summary is required");
        }
        if required(profile, AgentTaskReviewSectionId::WhatChanged) && self.what_changed.is_empty()
        {
            return invalid("what_changed", "what changed is required");
        }
        if self.how_to_test.is_empty() {
            return invalid("how_to_test", "How to test requires --test-step COMMAND=>EXPECTED, a recorded targeted command, or a manual reviewer instruction");
        }
        if required(profile, AgentTaskReviewSectionId::Compatibility)
            && self.compatibility.is_empty()
        {
            return invalid("compatibility", "compatibility is required");
        }
        if self.ai_assistance.used && placeholder_model(&self.ai_assistance.model) {
            return invalid(
                "ai_assistance.model",
                "AI disclosure requires a concrete model identifier",
            );
        }
        // `used_for` is the one introspective slot and must never render empty or
        // as a canned placeholder. This is the render-time boundary EVERY
        // finalization path crosses (cook's form gate is upstream of the cook
        // path only), so enforcing it here closes the hole for the manual
        // `agent-task review`/`pr` path too.
        if self.ai_assistance.used {
            if self.ai_assistance.used_for.trim().is_empty() {
                return invalid(
                    "ai_assistance.used_for",
                    "AI disclosure requires a concrete, self-reflective description of how AI was used (the `used_for` field); it cannot be empty",
                );
            }
            if used_for_is_placeholder(&self.ai_assistance.used_for) {
                return invalid(
                    "ai_assistance.used_for",
                    "AI disclosure `used_for` is a placeholder; provide a genuine, self-reflective description of the process the AI took",
                );
            }
        }
        Ok(())
    }
}

/// Reviewer-facing evidence label for a deterministic gate.
///
/// Gate `name` is a positional id (`gate-1`, `gate-2`) that is meaningless to a
/// reviewer. The gate `summary` carries the concrete, reveal-policy-safe
/// description instead — for a passing command gate it is
/// `"deterministic gate passed: <command>"`, and for a private/withheld gate it
/// is already redacted by policy. Prefer that; only fall back to the positional
/// id when no summary was recorded.
fn gate_evidence_summary(gate: &HomeboyGateResult) -> String {
    let summary = gate.summary.trim();
    if summary.is_empty() {
        format!("{}: {:?}", gate.name, gate.status)
    } else {
        format!("{summary} ({:?})", gate.status)
    }
}

pub fn enrich_dossier(
    dossier: &mut AgentTaskReviewDossier,
    source_refs: &[String],
    artifact_refs: &[String],
    gates: &[HomeboyGateResult],
    ci_expected: &[String],
    lifecycle: Option<&homeboy_core::run_lifecycle_record::RunLifecycleRecord>,
) {
    // Durable/operator evidence can be hydrated before finalization. Keep it in
    // the run report, but do not carry operator-only URLs into the reviewer form.
    dossier
        .evidence
        .retain(|evidence| !evidence.url.as_deref().is_some_and(operator_only_reference));
    for gate in gates {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: gate_evidence_summary(gate),
            url: None,
        });
    }
    for check in ci_expected {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("CI expected: {check}"),
            url: None,
        });
    }
    if let Some(lifecycle) = lifecycle {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("Durable run execution: {:?}", lifecycle.execution.state),
            url: None,
        });
    }
    for (provenance, reference) in source_refs
        .iter()
        .enumerate()
        .map(|(index, reference)| (format!("source_refs[{index}]"), reference))
        .chain(
            artifact_refs
                .iter()
                .enumerate()
                .map(|(index, reference)| (format!("artifact_refs[{index}]"), reference)),
        )
    {
        if is_reviewer_url(reference) {
            dossier.evidence.push(AgentTaskReviewEvidence {
                summary: format!("Reviewer-resolvable evidence from {provenance}"),
                url: Some(reference.clone()),
            });
        }
    }
    dossier
        .evidence
        .sort_by(|a, b| a.summary.cmp(&b.summary).then(a.url.cmp(&b.url)));
    dossier.evidence.dedup();
}

pub fn render_review_dossier(
    dossier: &AgentTaskReviewDossier,
    profile: &AgentTaskReviewProfile,
) -> String {
    let mut sections = Vec::new();
    for id in ordered_sections(profile) {
        if profile.hidden_sections.contains(&id) {
            continue;
        }
        let section = match id {
        AgentTaskReviewSectionId::Summary if !dossier.summary.is_empty() => Some(("Summary", prose(&dossier.summary))),
        AgentTaskReviewSectionId::WhatChanged if !dossier.what_changed.is_empty() => Some(("What changed", bullets(&dossier.what_changed))),
        AgentTaskReviewSectionId::HowToTest => {
            let steps = dossier
                .how_to_test
                .iter()
                .filter(|step| reviewer_runnable_command(&step.command))
                .collect::<Vec<_>>();
            (!steps.is_empty()).then(|| (
                "How to test",
                steps
                    .iter()
                    .enumerate()
                    .map(|(i, step)| format!("{}. Run `{}`; expect {}.", i + 1, code(&step.command), prose(&step.expected)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        }
        AgentTaskReviewSectionId::Compatibility if !dossier.compatibility.is_empty() => Some(("Compatibility", prose(&dossier.compatibility))),
        AgentTaskReviewSectionId::Evidence if !dossier.changed_public_contracts.is_empty() => Some(("Evidence", format!("{}{}", render_public_contract_evidence(dossier), render_evidence(&dossier.evidence)))),
        AgentTaskReviewSectionId::Evidence if !dossier.evidence.is_empty() => Some(("Evidence", dossier.evidence.iter().map(|item| match &item.url { Some(url) => format!("- {}: {url}", prose(&item.summary)), None => format!("- {}", prose(&item.summary)) }).collect::<Vec<_>>().join("\n"))),
        AgentTaskReviewSectionId::AiAssistance => Some(("AI assistance", format!("- **AI assistance:** {}\n- **Tool(s):** {}\n- **Model:** {}\n- **Used for:** {}", if dossier.ai_assistance.used { "Yes" } else { "No" }, prose(&dossier.ai_assistance.tool), prose(&dossier.ai_assistance.model), prose(&dossier.ai_assistance.used_for)))),
        AgentTaskReviewSectionId::SourceRelationships if !dossier.source_relationships.is_empty() => Some(("Source relationships", dossier.source_relationships.iter().map(|item| format!("- {} {}", match item.kind { AgentTaskReviewIssueRelationshipKind::Closes => "Closes", AgentTaskReviewIssueRelationshipKind::RelatesTo => "Relates to" }, relationship_reference(&item.reference))).collect::<Vec<_>>().join("\n"))), _ => None };
        if let Some((heading, content)) = section {
            sections.push(format!("## {heading}\n{content}"));
        }
    }
    for section in &profile.additional_sections {
        if !section.content.is_empty() {
            sections.push(format!(
                "## {}\n{}",
                section.heading,
                prose(&section.content)
            ));
        }
    }
    sections.join("\n\n") + "\n"
}
fn render_public_contract_evidence(dossier: &AgentTaskReviewDossier) -> String {
    let contracts = dossier
        .changed_public_contracts
        .iter()
        .map(|contract| format!("- `{}`: {}", code(&contract.id), prose(&contract.summary)))
        .collect::<Vec<_>>()
        .join("\n");
    let evidence = dossier
        .public_contract_evidence
        .as_ref()
        .expect("validated before rendering");
    let status = match &evidence.external_usage.status {
        AgentTaskExternalUsageStatus::Completed => "completed",
        AgentTaskExternalUsageStatus::UnavailableManualReview => "unavailable_manual_review",
    };
    format!(
        "**Declared public contracts**\n{contracts}\n\n**Compatibility impact:** {}\n\n**External-consumer impact:** {}\n\n**External usage:** {status}; source: {}; limitations: {}; evidence: {}\n\n",
        prose(&evidence.compatibility_impact),
        prose(&evidence.external_consumer_impact),
        prose(&evidence.external_usage.source),
        prose(&evidence.external_usage.limitations),
        evidence.external_usage.url,
    )
}
fn render_evidence(evidence: &[AgentTaskReviewEvidence]) -> String {
    evidence
        .iter()
        .map(|item| match &item.url {
            Some(url) => format!("- {}: {url}", prose(&item.summary)),
            None => format!("- {}", prose(&item.summary)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ordered_sections(profile: &AgentTaskReviewProfile) -> Vec<AgentTaskReviewSectionId> {
    let mut sections = profile.section_order.clone();
    for id in [
        AgentTaskReviewSectionId::Summary,
        AgentTaskReviewSectionId::WhatChanged,
        AgentTaskReviewSectionId::HowToTest,
        AgentTaskReviewSectionId::Compatibility,
        AgentTaskReviewSectionId::Evidence,
        AgentTaskReviewSectionId::AiAssistance,
        AgentTaskReviewSectionId::SourceRelationships,
    ] {
        if !sections.contains(&id) {
            sections.push(id);
        }
    }
    sections
}
fn required(profile: &AgentTaskReviewProfile, id: AgentTaskReviewSectionId) -> bool {
    profile.required_sections.contains(&id)
}
fn scalar(field: &str, value: &str) -> Result<()> {
    if value.contains(['\n', '\r'])
        || value.chars().any(char::is_control)
        || value.contains("<!--")
        || value.contains("</")
    {
        return invalid(
            field,
            "scalar content cannot contain control, newline, or closing markup",
        );
    }
    Ok(())
}
fn prose(value: &str) -> String {
    let mut rendered: String = reviewer_text(value)
        .chars()
        .flat_map(|character| match character {
            '*' | '_' | '`' | '[' | ']' | '<' | '>' | '!' => {
                vec!['\\', character]
            }
            _ => vec![character],
        })
        .collect();
    if rendered.starts_with(['#', '>', '-', '+'])
        || rendered
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
            && rendered.chars().nth(1) == Some('.')
    {
        rendered.insert(0, '\\');
    }
    rendered
}
fn code(value: &str) -> String {
    reviewer_text(value).replace('`', "'")
}

/// Reviewer-facing bodies must be independently resolvable. Omit operator-only
/// references rather than publishing local paths or durable runtime identifiers.
fn reviewer_text(value: &str) -> String {
    if contains_operator_only_reference(value) {
        "[operator-only reference omitted]".into()
    } else {
        value.into()
    }
}

pub fn reviewer_runnable_command(value: &str) -> bool {
    !value.trim().is_empty() && !contains_operator_only_reference(value)
}

fn contains_operator_only_reference(value: &str) -> bool {
    for word in value.split_whitespace() {
        let normalized = normalize_reviewer_token(word);
        if reviewer_credential_flag(normalized) {
            return true;
        }
        if operator_only_reference(normalized) {
            return true;
        }
    }
    false
}

fn normalize_reviewer_token(value: &str) -> &str {
    value.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | ',' | '.' | ':' | ';' | '\'' | '"'
        )
    })
}

fn reviewer_credential_flag(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "--token"
            | "--api-key"
            | "--api_key"
            | "--apikey"
            | "--secret"
            | "--password"
            | "--authorization"
            | "--auth-token"
            | "--access-token"
            | "--github-token"
    )
}

fn operator_only_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if let Some((name, assigned_value)) = value.split_once('=') {
        let name = name
            .trim_start_matches('-')
            .to_ascii_lowercase()
            .replace('-', "_");
        if [
            "token",
            "secret",
            "password",
            "apikey",
            "api_key",
            "authorization",
            "auth_token",
            "access_token",
            "github_token",
        ]
        .contains(&name.as_str())
            || operator_only_reference(normalize_reviewer_token(assigned_value))
        {
            return true;
        }
    }
    if lower.contains("localhost")
        || lower.starts_with("runner-artifact://")
        || lower.starts_with("artifact://")
        || lower.starts_with("homeboy://")
        || lower.starts_with("file://")
        || lower.starts_with('/')
        || lower.starts_with("~/")
        || lower.contains("/users/")
        || lower.contains("=/")
        || lower.contains("=~/")
        || [
            "token=",
            "secret=",
            "password=",
            "apikey=",
            "api_key=",
            "authorization=",
        ]
        .iter()
        .any(|secret| lower.contains(secret))
        || is_internal_run_id(value)
    {
        return true;
    }
    // `Url::parse` accepts colon-bearing CLI arguments as custom URI schemes.
    // Only hierarchical references are intended to be URLs in reviewer commands.
    if !value.contains("://") {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".test")
        || host.parse::<std::net::IpAddr>().is_ok()
}

fn is_internal_run_id(value: &str) -> bool {
    ["agent-task-", "cook-", "run-"].iter().any(|prefix| {
        value.starts_with(prefix)
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
            && value.chars().any(|character| character.is_ascii_digit())
    })
}
fn relationship_reference(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("https://github.com/") {
        if let Some((repository, number)) = rest.split_once("/issues/") {
            return format!("{repository}#{number}");
        }
    }
    value.to_string()
}
fn placeholder_model(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "not recorded"
            | "unknown"
            | "ai-assisted"
            | "ai assisted"
            | "legacy caller did not record a model"
    )
}
fn bullets(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("- {}", prose(value)))
        .collect::<Vec<_>>()
        .join("\n")
}
fn unique_sections(field: &str, values: &[AgentTaskReviewSectionId]) -> Result<()> {
    let mut unique = std::collections::BTreeSet::new();
    if values.iter().any(|value| !unique.insert(value)) {
        return invalid(field, "section identifiers must be unique");
    }
    Ok(())
}
fn builtin_section_name(id: &str) -> bool {
    matches!(
        id,
        "summary"
            | "what_changed"
            | "how_to_test"
            | "compatibility"
            | "evidence"
            | "ai_assistance"
            | "source_relationships"
    )
}
fn issue_pattern() -> Regex {
    Regex::new(r"^(?:#\d+|[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+#\d+|https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/issues/\d+)$").expect("valid issue regex")
}
pub fn validate_issue_reference(value: &str) -> Result<()> {
    if issue_pattern().is_match(value) {
        Ok(())
    } else {
        invalid(
            "source_relationships.reference",
            "issue references must be #number, owner/repo#number, or a github.com issue URL",
        )
    }
}
fn is_reviewer_url(value: &str) -> bool {
    value.starts_with("https://") && !operator_only_reference(value)
}
fn validate_reviewer_url(field: &str, value: &str, provenance: &str) -> Result<()> {
    let url = match reqwest::Url::parse(value) {
        Ok(url) => url,
        Err(_) => return invalid_reviewer_url(field, value, provenance, "is invalid"),
    };
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https" || operator_only_reference(value) || host.is_empty() {
        return invalid_reviewer_url(field, value, provenance, "must be a public HTTPS URL");
    }
    Ok(())
}
fn invalid_reviewer_url(field: &str, value: &str, provenance: &str, problem: &str) -> Result<()> {
    invalid(
        field,
        &format!("reviewer evidence URL `{value}` from {provenance} {problem}"),
    )
}
fn invalid(field: &str, message: &str) -> Result<()> {
    Err(Error::validation_invalid_argument(
        field, message, None, None,
    ))
}
fn dossier_schema() -> String {
    AGENT_TASK_REVIEW_DOSSIER_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dossier() -> AgentTaskReviewDossier {
        AgentTaskReviewDossier {
            schema: dossier_schema(),
            summary: "Add dossier".into(),
            what_changed: vec!["Changes output".into()],
            how_to_test: vec![AgentTaskReviewTestStep {
                command: "cargo test dossier".into(),
                expected: "passes".into(),
            }],
            compatibility: "No compatibility impact".into(),
            evidence: Vec::new(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            ai_assistance: AgentTaskReviewAiAssistance {
                used: true,
                tool: "OpenCode".into(),
                model: "openai/gpt-5.6-terra".into(),
                used_for: "Isolated the failing path, implemented the guard, and verified with the recorded gate before finalizing.".into(),
            },
            source_relationships: vec![AgentTaskReviewIssueRelationship {
                kind: AgentTaskReviewIssueRelationshipKind::Closes,
                reference: "#8058".into(),
            }],
            overrides: Vec::new(),
        }
    }

    fn valid_form() -> AiFilledReviewForm {
        AiFilledReviewForm {
            summary: "Fix the widget so it renders on reload.".into(),
            what_changed: vec!["Guard the null path in render().".into()],
            compatibility: "No compatibility impact; internal only.".into(),
            used_for: "I traced the null deref to the reload path, added a guard, and verified with a focused test before finalizing.".into(),
        }
    }

    #[test]
    fn valid_review_form_passes_validation() {
        assert!(valid_form().validate().is_ok());
    }

    #[test]
    fn review_form_rejects_empty_required_fields() {
        for mutate in [
            (|f: &mut AiFilledReviewForm| f.summary.clear()) as fn(&mut AiFilledReviewForm),
            |f| f.what_changed.clear(),
            |f| f.compatibility.clear(),
            |f| f.used_for.clear(),
        ] {
            let mut form = valid_form();
            mutate(&mut form);
            assert!(
                form.validate().is_err(),
                "expected validation failure for cleared required field"
            );
        }
    }

    #[test]
    fn review_form_rejects_placeholder_used_for() {
        let mut form = valid_form();
        form.used_for =
            "Drafted implementation and tests; Chris reviews and owns the change.".into();
        let error = form
            .validate()
            .expect_err("legacy canned default must be rejected");
        assert!(error.message.contains("placeholder"));
    }

    #[test]
    fn review_form_rejects_used_for_equal_to_summary() {
        let mut form = valid_form();
        form.used_for = form.summary.clone();
        let error = form
            .validate()
            .expect_err("used_for restating summary is not a process reflection");
        assert!(error.message.contains("distinct from summary"));
    }

    #[test]
    fn review_form_parses_from_outcome_outputs() {
        let outputs = serde_json::json!({ "review_form": valid_form() });
        let parsed = AiFilledReviewForm::from_outcome_outputs(&outputs)
            .expect("parse")
            .expect("present");
        assert_eq!(parsed, valid_form());
    }

    #[test]
    fn absent_review_form_parses_as_none() {
        assert!(
            AiFilledReviewForm::from_outcome_outputs(&serde_json::json!({}))
                .expect("parse")
                .is_none()
        );
        assert!(AiFilledReviewForm::from_outcome_outputs(
            &serde_json::json!({ "review_form": null })
        )
        .expect("parse")
        .is_none());
    }

    #[test]
    fn malformed_review_form_is_a_hard_error() {
        let outputs = serde_json::json!({ "review_form": { "summary": 42 } });
        assert!(AiFilledReviewForm::from_outcome_outputs(&outputs).is_err());
    }

    #[test]
    fn gate_evidence_summary_uses_command_bearing_summary_not_positional_id() {
        let gate = HomeboyGateResult::new(
            "gate-1",
            "gate-1",
            homeboy_core::gate::HomeboyGateKind::Command,
            homeboy_core::gate::HomeboyGateStatus::Passed,
        )
        .summary("deterministic gate passed: cargo test widget");
        let label = gate_evidence_summary(&gate);
        assert!(
            label.contains("cargo test widget"),
            "expected the command in the label, got: {label}"
        );
        assert!(
            !label.starts_with("gate-1"),
            "must not lead with the positional id"
        );
    }

    #[test]
    fn gate_evidence_summary_falls_back_to_id_when_summary_absent() {
        let gate = HomeboyGateResult::new(
            "gate-2",
            "gate-2",
            homeboy_core::gate::HomeboyGateKind::Command,
            homeboy_core::gate::HomeboyGateStatus::Passed,
        );
        assert_eq!(gate_evidence_summary(&gate), "gate-2: Passed");
    }

    #[test]
    fn renderer_is_deterministic_and_safe() {
        let body = render_review_dossier(&dossier(), &default_profile());
        assert!(body.starts_with("## Summary"));
        assert!(body.contains("1. Run `cargo test dossier`; expect passes."));
        assert!(!body.contains("Publication intent"));
    }
    #[test]
    fn renderer_uses_github_closing_syntax_and_escapes_structural_markdown() {
        let mut value = dossier();
        value.summary =
            "# heading > quote - list 1. ordered ``` fence [link](x) <!-- comment".into();
        value
            .source_relationships
            .push(AgentTaskReviewIssueRelationship {
                kind: AgentTaskReviewIssueRelationshipKind::RelatesTo,
                reference: "owner/repo#9".into(),
            });
        let body = render_review_dossier(&value, &default_profile());
        assert!(body.contains("Closes #8058"));
        assert!(body.contains("Relates to owner/repo#9"));
        assert!(!body.contains("Closes: #8058"));
        assert!(body.contains("\\# heading \\> quote - list 1. ordered"));
    }
    #[test]
    fn overrides_apply_and_keep_provenance() {
        let mut value = dossier();
        value.overrides.push(AgentTaskReviewOverride {
            target: AgentTaskReviewOverrideTarget::Summary,
            value: "Override".into(),
            provenance: "operator".into(),
        });
        value.apply_overrides().unwrap();
        assert_eq!(value.summary, "Override");
        assert_eq!(value.overrides[0].provenance, "operator");
    }
    #[test]
    fn rejects_injection_and_bad_issue_refs() {
        let mut value = dossier();
        value.summary = "ok\n## injected".into();
        assert!(value.validate(&default_profile()).is_err());
        let mut value = dossier();
        value.source_relationships[0].reference = "https://evil.test/issues/1".into();
        assert!(value.validate(&default_profile()).is_err());
    }
    #[test]
    fn dossier_validate_rejects_empty_or_placeholder_used_for_on_every_path() {
        // Regression for #9649: the manual finalize path rendered an empty
        // `Used for:` because used_for enforcement only lived in the cook form
        // gate. The render-time dossier gate must reject it regardless of path.
        let mut empty = dossier();
        empty.ai_assistance.used_for = "  ".into();
        assert!(
            empty.validate(&default_profile()).is_err(),
            "empty used_for must fail finalization"
        );

        let mut placeholder = dossier();
        placeholder.ai_assistance.used_for =
            "Drafted implementation and tests; Chris reviews and owns the change.".into();
        assert!(
            placeholder.validate(&default_profile()).is_err(),
            "canned placeholder used_for must fail finalization"
        );
    }

    #[test]
    fn homeboy_tool_disclosure_attributes_the_orchestrator() {
        assert_eq!(
            homeboy_tool_disclosure("OpenCode (openai/gpt-5.6-terra)"),
            "Homeboy (OpenCode (openai/gpt-5.6-terra))"
        );
        // Idempotent and empty-safe.
        assert_eq!(
            homeboy_tool_disclosure("Homeboy (OpenCode)"),
            "Homeboy (OpenCode)"
        );
        assert_eq!(homeboy_tool_disclosure(""), "");
    }

    #[test]
    fn profile_conflicts_fail_closed() {
        let profile = AgentTaskReviewProfile {
            required_sections: vec![AgentTaskReviewSectionId::Summary],
            hidden_sections: vec![AgentTaskReviewSectionId::Summary],
            ..Default::default()
        };
        assert!(validate_profile(&profile).is_err());
    }
    #[test]
    fn url_policy_rejects_local_urls() {
        let mut value = dossier();
        value.evidence.push(AgentTaskReviewEvidence {
            summary: "local".into(),
            url: Some("https://localhost/a".into()),
        });
        assert!(value.validate(&default_profile()).is_err());
    }

    #[test]
    fn reviewer_url_diagnostic_identifies_the_url_and_its_provenance() {
        let mut value = dossier();
        value.evidence.push(AgentTaskReviewEvidence {
            summary: "durable artifact retained for operators".into(),
            url: Some("homeboy://agent-task/run/run-9568/artifacts".into()),
        });

        let error = value
            .validate(&default_profile())
            .expect_err("internal URL is not reviewer-resolvable");

        assert!(error
            .message
            .contains("homeboy://agent-task/run/run-9568/artifacts"));
        assert!(error
            .message
            .contains("durable artifact retained for operators"));
    }

    #[test]
    fn public_contract_url_diagnostic_identifies_the_url_and_source() {
        let mut value = dossier();
        value
            .changed_public_contracts
            .push(AgentTaskPublicContract {
                id: "api.widget.render".into(),
                summary: "Changes the rendered result.".into(),
            });
        let mut evidence = public_contract_evidence();
        evidence.external_usage.source = "Internal durable artifact".into();
        evidence.external_usage.url = "homeboy://agent-task/run/run-9568/artifacts".into();
        value.public_contract_evidence = Some(evidence);

        let error = value
            .validate(&default_profile())
            .expect_err("internal public-contract URL is not reviewer-resolvable");

        assert!(error
            .message
            .contains("homeboy://agent-task/run/run-9568/artifacts"));
        assert!(error.message.contains("Internal durable artifact"));
    }

    fn public_contract_evidence() -> AgentTaskPublicContractEvidence {
        AgentTaskPublicContractEvidence {
            compatibility_impact: "Existing callers retain their supported behavior.".into(),
            external_consumer_impact: "External consumers must review the declared change.".into(),
            external_usage: AgentTaskExternalUsageEvidence {
                status: AgentTaskExternalUsageStatus::Completed,
                source: "Repository-wide usage search".into(),
                limitations: "Search only covers the indexed source repositories.".into(),
                url: "https://github.com/example/project/issues/1".into(),
            },
        }
    }

    #[test]
    fn declared_public_contract_requires_complete_reviewer_evidence() {
        let mut value = dossier();
        value
            .changed_public_contracts
            .push(AgentTaskPublicContract {
                id: "api.widget.render".into(),
                summary: "Changes the rendered result.".into(),
            });
        assert!(value.validate(&default_profile()).is_err());

        value.public_contract_evidence = Some(public_contract_evidence());
        value
            .validate(&default_profile())
            .expect("complete evidence");
        assert!(
            render_review_dossier(&value, &default_profile()).contains("unavailable_manual_review")
                == false
        );
    }

    #[test]
    fn public_contract_evidence_rejects_malformed_or_local_only_proof() {
        let mut value = dossier();
        value
            .changed_public_contracts
            .push(AgentTaskPublicContract {
                id: "api.widget.render".into(),
                summary: "Changes the rendered result.".into(),
            });
        let mut evidence = public_contract_evidence();
        evidence.external_usage.source.clear();
        value.public_contract_evidence = Some(evidence);
        assert!(value.validate(&default_profile()).is_err());

        let mut evidence = public_contract_evidence();
        evidence.external_usage.url = "https://localhost/evidence".into();
        value.public_contract_evidence = Some(evidence);
        assert!(value.validate(&default_profile()).is_err());
    }

    #[test]
    fn unavailable_manual_review_is_accepted_with_durable_evidence() {
        let mut value = dossier();
        value
            .changed_public_contracts
            .push(AgentTaskPublicContract {
                id: "api.widget.render".into(),
                summary: "Changes the rendered result.".into(),
            });
        let mut evidence = public_contract_evidence();
        evidence.external_usage.status = AgentTaskExternalUsageStatus::UnavailableManualReview;
        value.public_contract_evidence = Some(evidence);
        value
            .validate(&default_profile())
            .expect("manual review outcome");
        assert!(
            render_review_dossier(&value, &default_profile()).contains("unavailable_manual_review")
        );
    }

    #[test]
    fn internal_only_changes_do_not_require_public_contract_evidence() {
        dossier()
            .validate(&default_profile())
            .expect("internal-only change");
    }

    #[test]
    fn test_instructions_are_required_even_when_profile_does_not_require_them() {
        let mut value = dossier();
        value.how_to_test.clear();
        let profile = AgentTaskReviewProfile {
            required_sections: vec![AgentTaskReviewSectionId::Summary],
            ..Default::default()
        };
        assert!(value.validate(&profile).is_err());

        let hidden = AgentTaskReviewProfile {
            hidden_sections: vec![AgentTaskReviewSectionId::HowToTest],
            ..Default::default()
        };
        assert!(validate_profile(&hidden).is_err());
    }

    #[test]
    fn renderer_omits_operator_only_references() {
        let mut value = dossier();
        value.summary = "Inspect http://localhost:8888 at /Users/chris/repo using runner-artifact://gate and agent-task-1234".into();
        value.how_to_test[0].command =
            "cargo test --manifest-path /tmp/repo/Cargo.toml file:///private/repo".into();
        value.evidence.push(AgentTaskReviewEvidence {
            summary:
                "Durable evidence homeboy://agent-task/run/agent-task-1234 from cook-8058-attempt-1"
                    .into(),
            url: None,
        });

        let body = render_review_dossier(&value, &default_profile());

        for forbidden in [
            "localhost",
            "/Users/chris/repo",
            "runner-artifact://",
            "homeboy://",
            "agent-task-1234",
            "cook-8058-attempt-1",
            "/tmp/repo",
            "file:///private/repo",
        ] {
            assert!(!body.contains(forbidden), "leaked {forbidden}: {body}");
        }
        assert!(!body.contains("## How to test"));
    }

    #[test]
    fn reviewer_sanitization_rejects_assignment_values_and_space_delimited_credentials() {
        for value in [
            "root=/private/repo",
            "path=~/workspace",
            "BASE_URL=http://127.0.0.1:8080 cargo test",
            "--source=file:///private/repo cargo test",
            "API_URL=https://token@example.com/path cargo test",
            "--token ghp_secret cargo test",
            "https://token@example.com/evidence",
            "https://example.com/evidence?token=secret",
            "--report=https://example.com/evidence?api_key=secret cargo test",
            "https://10.0.0.1/evidence",
            "https://192.168.1.1/evidence",
            "https://172.16.0.1/evidence",
            "https://review.internal/evidence",
            "https://review.local/evidence",
            "https://review.test/evidence",
            "API_KEY=secret cargo test",
        ] {
            assert!(!reviewer_runnable_command(value), "accepted {value}");
            assert_eq!(
                reviewer_text(value),
                "[operator-only reference omitted]",
                "rendered unsafe reviewer claim: {value}"
            );
        }

        let mut value = dossier();
        value.evidence.push(AgentTaskReviewEvidence {
            summary: "private".into(),
            url: Some("https://example.com/evidence?token=secret".into()),
        });
        assert!(value.validate(&default_profile()).is_err());
    }

    #[test]
    fn reviewer_commands_allow_colon_bearing_tokens_but_reject_url_references() {
        for value in [
            "npm run test:browser-scenarios",
            "mvn verify -DskipTests=false:test",
            "cargo test feature:browser-scenarios",
        ] {
            assert!(reviewer_runnable_command(value), "rejected {value}");
        }

        for value in [
            "curl http://example.com/evidence",
            "curl https://localhost:8888/evidence",
            "cargo test file:///private/repo",
            "cargo test --token secret",
        ] {
            assert!(!reviewer_runnable_command(value), "accepted {value}");
        }
    }

    #[test]
    fn configured_profile_is_loaded_canonically_and_invalid_policy_fails_closed() {
        let directory = tempfile::tempdir().expect("temporary component");
        std::fs::write(
            directory.path().join("homeboy.json"),
            r#"{"id":"review-profile-test","review_profile":{"required_sections":["summary"],"hidden_sections":["summary"]}}"#,
        )
        .expect("portable config");
        assert!(resolve_review_profile(directory.path().to_str().expect("path")).is_err());
    }
}
