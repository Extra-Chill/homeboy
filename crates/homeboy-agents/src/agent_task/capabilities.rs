use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA: &str =
    "homeboy/agent-task-capability-requirements/v1";
pub const AGENT_TASK_CAPABILITY_EVIDENCE_SCHEMA: &str = "homeboy/agent-task-capability-evidence/v1";

/// Capability declarations are deliberately split by the actor that can satisfy
/// them. Provider capabilities cannot be satisfied by a runner or an attached
/// tool, and runner capabilities cannot be satisfied by a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCapabilityRequirements {
    #[serde(default = "capability_requirements_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_tools: Vec<AgentTaskAttachedToolCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAttachedToolCapability {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributes: Vec<String>,
}

/// Durable evidence is append-only: declarations are requested inputs, while
/// advertised and contributed values are observations made by their owners.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCapabilityEvidence {
    #[serde(default = "capability_evidence_schema")]
    pub schema: String,
    #[serde(default)]
    pub requested: AgentTaskCapabilityRequirements,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_advertised: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_advertised: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_contributed: Vec<AgentTaskToolCapabilityContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AgentTaskCapabilityAdmission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCapabilityAdmission {
    pub status: String,
    pub layer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskToolCapabilityContribution {
    pub tool_id: String,
    pub capabilities: Vec<String>,
    pub readiness: String,
}

pub(crate) fn ready_attached_tools_from_metadata(metadata: &Value) -> BTreeSet<String> {
    metadata
        .get("attached_tool_readiness")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|entries| entries.iter())
        .filter(|&(_id, readiness)| readiness.get("state").and_then(Value::as_str) == Some("ready"))
        .map(|(id, _readiness)| id.clone())
        .collect()
}

impl AgentTaskCapabilityRequirements {
    pub fn normalized(mut self) -> Self {
        normalize(&mut self.provider);
        normalize(&mut self.runner);
        for tool in &mut self.attached_tools {
            tool.id = tool.id.trim().to_string();
            normalize(&mut tool.contributes);
        }
        self.attached_tools
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.attached_tools
            .dedup_by(|left, right| left.id == right.id);
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        for (layer, capabilities) in [("provider", &self.provider), ("runner", &self.runner)] {
            if capabilities
                .iter()
                .any(|capability| capability.trim().is_empty())
            {
                return Err(format!(
                    "{layer} capability requirements contain an empty capability"
                ));
            }
        }
        if self.attached_tools.iter().any(|tool| tool.id.is_empty()) {
            return Err(
                "attached tool capability requirements contain an empty tool id".to_string(),
            );
        }
        Ok(())
    }

    pub fn evidence(
        &self,
        provider_advertised: impl IntoIterator<Item = String>,
        runner_advertised: impl IntoIterator<Item = String>,
        ready_tools: &BTreeSet<String>,
    ) -> AgentTaskCapabilityEvidence {
        let requested = self.clone().normalized();
        let mut provider_advertised = provider_advertised.into_iter().collect::<Vec<_>>();
        let mut runner_advertised = runner_advertised.into_iter().collect::<Vec<_>>();
        normalize(&mut provider_advertised);
        normalize(&mut runner_advertised);
        let tool_contributed = requested
            .attached_tools
            .iter()
            .filter(|tool| ready_tools.contains(&tool.id))
            .map(|tool| AgentTaskToolCapabilityContribution {
                tool_id: tool.id.clone(),
                capabilities: tool.contributes.clone(),
                readiness: "ready".to_string(),
            })
            .collect::<Vec<_>>();
        let mut resolved = provider_advertised.clone();
        resolved.extend(runner_advertised.clone());
        resolved.extend(
            tool_contributed
                .iter()
                .flat_map(|tool| tool.capabilities.clone()),
        );
        normalize(&mut resolved);
        AgentTaskCapabilityEvidence {
            schema: capability_evidence_schema(),
            requested,
            provider_advertised,
            runner_advertised,
            tool_contributed,
            resolved,
            admission: None,
        }
    }
}

pub fn requirements_from_metadata(
    metadata: &Value,
    legacy_provider: &[String],
) -> Result<AgentTaskCapabilityRequirements, String> {
    let Some(value) = metadata.get("capability_requirements") else {
        // v1 had one executor-owned field. This is an explicit migration, not a
        // cross-layer inference: old values remain provider requirements only.
        return Ok(AgentTaskCapabilityRequirements {
            schema: AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA.to_string(),
            provider: legacy_provider.to_vec(),
            ..Default::default()
        }
        .normalized());
    };
    let requirements: AgentTaskCapabilityRequirements = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid capability_requirements: {error}"))?;
    if requirements.schema != AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA {
        return Err(format!(
            "unsupported capability requirements schema '{}'",
            requirements.schema
        ));
    }
    let requirements = requirements.normalized();
    requirements.validate()?;
    Ok(requirements)
}

fn normalize(values: &mut Vec<String>) {
    values.retain_mut(|value| {
        *value = value.trim().to_string();
        !value.is_empty()
    });
    values.sort();
    values.dedup();
}

fn capability_requirements_schema() -> String {
    AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA.to_string()
}

fn capability_evidence_schema() -> String {
    AGENT_TASK_CAPABILITY_EVIDENCE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_v1_executor_requirements_only_to_the_provider_layer() {
        let requirements = requirements_from_metadata(&Value::Null, &["structured_outcome".into()])
            .expect("v1 migration");
        assert_eq!(requirements.provider, ["structured_outcome"]);
        assert!(requirements.runner.is_empty());
    }

    #[test]
    fn only_ready_attached_tools_contribute_capabilities() {
        let requirements: AgentTaskCapabilityRequirements = serde_json::from_value(json!({
            "schema": AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA,
            "runner": ["browser"],
            "attached_tools": [{ "id": "browser-mcp", "contributes": ["browser_control"] }]
        }))
        .expect("requirements");
        let not_ready = requirements.evidence(Vec::new(), vec!["browser".into()], &BTreeSet::new());
        assert!(!not_ready.resolved.contains(&"browser_control".to_string()));
        let ready = requirements.evidence(
            Vec::new(),
            vec!["browser".into()],
            &["browser-mcp".to_string()].into_iter().collect(),
        );
        assert_eq!(ready.tool_contributed[0].readiness, "ready");
        assert!(ready.resolved.contains(&"browser_control".to_string()));
    }
}
