use super::*;
use std::fmt;
use std::str::FromStr;

pub const AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS: &str = "mutation_artifacts";
pub const RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA: &str =
    "homeboy/resolved-agent-runtime-execution-contract/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAgentRuntimeExecutionContract {
    #[serde(default = "resolved_agent_runtime_execution_contract_schema")]
    pub schema: String,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_materialization: Option<ResolvedAgentRuntimeWorkspaceMaterializationSummary>,
    #[serde(
        default,
        skip_serializing_if = "ResolvedAgentRuntimeSecretEnvPlan::is_empty"
    )]
    pub secret_env_plan: ResolvedAgentRuntimeSecretEnvPlan,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResolvedAgentRuntimeWorkspaceMaterializationSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<WorkspaceMaterializationSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<WorkspaceMountSpec>,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeApplyBack::is_empty")]
    pub apply_back: AgentTaskRuntimeApplyBack,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResolvedAgentRuntimeSecretEnvPlan {
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<SecretEnvPlan>,
}

impl ResolvedAgentRuntimeSecretEnvPlan {
    pub fn is_empty(&self) -> bool {
        self.plan_ref.is_none() && self.object.is_none()
    }
}

fn resolved_agent_runtime_execution_contract_schema() -> String {
    RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTaskApplyBackStrategy {
    MutationArtifacts,
}

impl AgentTaskApplyBackStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MutationArtifacts => AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS,
        }
    }
}

impl fmt::Display for AgentTaskApplyBackStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AgentTaskApplyBackStrategy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS => Ok(Self::MutationArtifacts),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeApplyBack {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_artifacts: Vec<AgentTaskRuntimeMutationArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git_checkout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

impl AgentTaskRuntimeApplyBack {
    pub(super) fn is_empty(&self) -> bool {
        self.mutation_artifacts.is_empty()
            && self.requires_git_checkout.is_none()
            && self.strategy.is_none()
    }

    pub fn strategy(&self) -> Option<AgentTaskApplyBackStrategy> {
        self.strategy
            .as_deref()
            .and_then(|value| AgentTaskApplyBackStrategy::from_str(value).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_runtime_execution_contract_serializes_canonical_shape() {
        let contract = ResolvedAgentRuntimeExecutionContract {
            schema: RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA.to_string(),
            provider_id: "provider-1".to_string(),
            provider_backend: Some("example-backend".to_string()),
            runtime_id: Some("runtime-1".to_string()),
            runtime_path: Some("/runtime/path".to_string()),
            workspace_materialization: Some(ResolvedAgentRuntimeWorkspaceMaterializationSummary {
                cwd: Some(WorkspaceCwdMode::GitCheckout.to_string()),
                requires_git: Some(true),
                write_scope: Some(WorkspaceWriteScope::Workspace.to_string()),
                artifact_paths: vec!["artifacts".to_string()],
                ..ResolvedAgentRuntimeWorkspaceMaterializationSummary::default()
            }),
            secret_env_plan: ResolvedAgentRuntimeSecretEnvPlan {
                plan_ref: Some("runner-artifact://runner/run/secret-env-plan.json".to_string()),
                object: Some(SecretEnvPlan::from_secret_env_names([
                    "EXAMPLE_TOKEN".to_string()
                ])),
            },
            capabilities: vec!["apply_back".to_string()],
            extra: BTreeMap::new(),
        };

        let value = serde_json::to_value(&contract).expect("serialize contract");

        assert_eq!(
            value["schema"],
            RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA
        );
        assert_eq!(value["provider_id"], "provider-1");
        assert_eq!(value["workspace_materialization"]["cwd"], "git_checkout");
        assert_eq!(
            value["secret_env_plan"]["ref"],
            "runner-artifact://runner/run/secret-env-plan.json"
        );
        assert_eq!(
            value["secret_env_plan"]["object"]["secret_env_names"][0],
            "EXAMPLE_TOKEN"
        );
        assert_eq!(value["capabilities"][0], "apply_back");
    }

    #[test]
    fn resolved_runtime_execution_contract_defaults_schema_and_omits_empty_sections() {
        let contract = serde_json::from_value::<ResolvedAgentRuntimeExecutionContract>(
            serde_json::json!({ "provider_id": "provider-1" }),
        )
        .expect("deserialize minimal contract");

        assert_eq!(
            contract.schema,
            RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA
        );
        assert!(contract.secret_env_plan.is_empty());

        let value = serde_json::to_value(&contract).expect("serialize minimal contract");
        assert!(value.get("secret_env_plan").is_none());
        assert!(value.get("workspace_materialization").is_none());
    }

    #[test]
    fn apply_back_strategy_parses_known_contract_value() {
        let apply_back = AgentTaskRuntimeApplyBack {
            strategy: Some(AgentTaskApplyBackStrategy::MutationArtifacts.to_string()),
            ..AgentTaskRuntimeApplyBack::default()
        };

        assert_eq!(
            apply_back.strategy(),
            Some(AgentTaskApplyBackStrategy::MutationArtifacts)
        );
    }

    #[test]
    fn apply_back_strategy_keeps_unknown_strings_non_breaking() {
        let apply_back = AgentTaskRuntimeApplyBack {
            strategy: Some("provider_owned_strategy".to_string()),
            ..AgentTaskRuntimeApplyBack::default()
        };

        assert_eq!(apply_back.strategy(), None);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeMutationArtifact {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_method: Option<String>,
}
