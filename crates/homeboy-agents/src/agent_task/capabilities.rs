use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA: &str =
    "homeboy/agent-task-capability-requirements/v1";
pub const AGENT_TASK_CAPABILITY_EVIDENCE_SCHEMA: &str = "homeboy/agent-task-capability-evidence/v1";

/// Capability declarations are deliberately split by the actor that can satisfy
/// them. Provider capabilities cannot be satisfied by a runner, and runner
/// capabilities cannot be satisfied by a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCapabilityRequirements {
    #[serde(default = "capability_requirements_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner: Vec<String>,
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

impl AgentTaskCapabilityRequirements {
    pub fn normalized(mut self) -> Self {
        normalize(&mut self.provider);
        normalize(&mut self.runner);
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
        Ok(())
    }

    pub fn evidence(
        &self,
        provider_advertised: impl IntoIterator<Item = String>,
        runner_advertised: impl IntoIterator<Item = String>,
    ) -> AgentTaskCapabilityEvidence {
        let requested = self.clone().normalized();
        let mut provider_advertised = provider_advertised.into_iter().collect::<Vec<_>>();
        let mut runner_advertised = runner_advertised.into_iter().collect::<Vec<_>>();
        normalize(&mut provider_advertised);
        normalize(&mut runner_advertised);
        let mut resolved = provider_advertised.clone();
        resolved.extend(runner_advertised.clone());
        normalize(&mut resolved);
        AgentTaskCapabilityEvidence {
            schema: capability_evidence_schema(),
            requested,
            provider_advertised,
            runner_advertised,
            resolved,
            admission: None,
        }
    }
}

pub fn requirements_from_metadata(
    metadata: &Value,
    executor_required: &[String],
) -> Result<AgentTaskCapabilityRequirements, String> {
    let Some(value) = metadata.get("capability_requirements") else {
        // Without structured requirements, the executor-required capabilities
        // are provider requirements only (no cross-layer inference).
        return Ok(AgentTaskCapabilityRequirements {
            schema: AGENT_TASK_CAPABILITY_REQUIREMENTS_SCHEMA.to_string(),
            provider: executor_required.to_vec(),
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

    #[test]
    fn migrates_v1_executor_requirements_only_to_the_provider_layer() {
        let requirements = requirements_from_metadata(&Value::Null, &["structured_outcome".into()])
            .expect("v1 migration");
        assert_eq!(requirements.provider, ["structured_outcome"]);
        assert!(requirements.runner.is_empty());
    }
}
