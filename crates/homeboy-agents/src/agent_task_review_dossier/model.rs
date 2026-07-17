use serde::{Deserialize, Serialize};

use super::AGENT_TASK_REVIEW_DOSSIER_SCHEMA;

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

fn dossier_schema() -> String {
    AGENT_TASK_REVIEW_DOSSIER_SCHEMA.to_string()
}
