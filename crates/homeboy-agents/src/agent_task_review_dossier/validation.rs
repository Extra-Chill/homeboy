use regex::Regex;

use homeboy_core::error::{Error, Result};

use super::model::{
    AgentTaskReviewDossier, AgentTaskReviewOverrideTarget, AgentTaskReviewProfile,
    AgentTaskReviewSectionId,
};
use super::AGENT_TASK_REVIEW_DOSSIER_SCHEMA;

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
