use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::gate::HomeboyGateResult;

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
pub struct AgentTaskReviewDossier {
    #[serde(default = "dossier_schema")]
    pub schema: String,
    pub summary: String,
    pub what_changed: Vec<String>,
    pub how_to_test: Vec<AgentTaskReviewTestStep>,
    pub compatibility: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AgentTaskReviewEvidence>,
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
    let component = crate::component::resolve_effective(None, Some(path), None)?;
    // The component model carries the profile opaquely as JSON; deserialize it
    // here (the agent-task layer owns the profile schema). A present-but-invalid
    // profile fails finalization instead of being mistaken for profile absence.
    let profile = match component.review_profile {
        Some(value) => serde_json::from_value(value).map_err(|error| {
            crate::Error::validation_invalid_json(
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
        }
        for evidence in &self.evidence {
            scalar("evidence.summary", &evidence.summary)?;
            if let Some(url) = &evidence.url {
                validate_reviewer_url(url)?;
            }
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
        if required(profile, AgentTaskReviewSectionId::HowToTest) && self.how_to_test.is_empty() {
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
        Ok(())
    }
}

pub fn enrich_dossier(
    dossier: &mut AgentTaskReviewDossier,
    source_refs: &[String],
    artifact_refs: &[String],
    gates: &[HomeboyGateResult],
    ci_expected: &[String],
    lifecycle: Option<&crate::run_lifecycle_record::RunLifecycleRecord>,
) {
    for gate in gates {
        dossier.evidence.push(AgentTaskReviewEvidence {
            summary: format!("{}: {:?}", gate.name, gate.status),
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
    for reference in source_refs.iter().chain(artifact_refs) {
        if is_reviewer_url(reference) {
            dossier.evidence.push(AgentTaskReviewEvidence {
                summary: "Reviewer-resolvable source evidence".to_string(),
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
        AgentTaskReviewSectionId::HowToTest if !dossier.how_to_test.is_empty() => Some(("How to test", dossier.how_to_test.iter().enumerate().map(|(i, step)| format!("{}. Run `{}`; expect {}.", i + 1, code(&step.command), prose(&step.expected))).collect::<Vec<_>>().join("\n"))),
        AgentTaskReviewSectionId::Compatibility if !dossier.compatibility.is_empty() => Some(("Compatibility", prose(&dossier.compatibility))),
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
    let mut rendered: String = value
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
    value.replace('`', "'")
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
fn validate_issue_reference(value: &str) -> Result<()> {
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
    value.starts_with("https://")
}
fn validate_reviewer_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| {
        Error::validation_invalid_argument("evidence.url", "evidence URL is invalid", None, None)
    })?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(is_non_public_ip)
            .unwrap_or(false)
    {
        return invalid("evidence.url", "evidence URL must be a public HTTPS URL");
    }
    Ok(())
}
fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified(),
    }
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
            ai_assistance: AgentTaskReviewAiAssistance {
                used: true,
                tool: "OpenCode".into(),
                model: "openai/gpt-5.6-terra".into(),
                used_for: "Implementation".into(),
            },
            source_relationships: vec![AgentTaskReviewIssueRelationship {
                kind: AgentTaskReviewIssueRelationshipKind::Closes,
                reference: "#8058".into(),
            }],
            overrides: Vec::new(),
        }
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
