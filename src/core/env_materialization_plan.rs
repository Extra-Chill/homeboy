use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const ENV_MATERIALIZATION_PLAN_SCHEMA: &str = "homeboy/env-materialization-plan/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvMaterializationPlan {
    #[serde(default = "env_materialization_plan_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub public_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<EnvSecretRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_env_bindings: Vec<EnvSourceEnvBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_allowed_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_handoff: Option<EnvMaterializedHandoffMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvSecretRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvSourceEnvBinding {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_env_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvMaterializedHandoffMetadata {
    pub handoff_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_names: Vec<String>,
}

impl Default for EnvMaterializationPlan {
    fn default() -> Self {
        Self {
            schema: ENV_MATERIALIZATION_PLAN_SCHEMA.to_string(),
            public_env: BTreeMap::new(),
            secret_refs: Vec::new(),
            source_env_bindings: Vec::new(),
            inherited_allowed_env_names: Vec::new(),
            materialized_handoff: None,
        }
    }
}

impl EnvMaterializationPlan {
    pub fn is_empty(&self) -> bool {
        self.public_env.is_empty()
            && self.secret_refs.is_empty()
            && self.source_env_bindings.is_empty()
            && self.inherited_allowed_env_names.is_empty()
            && self.materialized_handoff.is_none()
    }

    pub fn normalize(&mut self) {
        self.schema = ENV_MATERIALIZATION_PLAN_SCHEMA.to_string();
        self.secret_refs = normalize_secret_refs(std::mem::take(&mut self.secret_refs));
        self.source_env_bindings =
            normalize_source_env_bindings(std::mem::take(&mut self.source_env_bindings));
        self.inherited_allowed_env_names =
            normalize_names(std::mem::take(&mut self.inherited_allowed_env_names));
    }
}

fn env_materialization_plan_schema() -> String {
    ENV_MATERIALIZATION_PLAN_SCHEMA.to_string()
}

fn normalize_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalize_secret_refs(refs: impl IntoIterator<Item = EnvSecretRef>) -> Vec<EnvSecretRef> {
    refs.into_iter()
        .filter_map(|secret_ref| {
            let name = secret_ref.name.trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some((
                    name.clone(),
                    EnvSecretRef {
                        name,
                        owner: secret_ref
                            .owner
                            .map(|owner| owner.trim().to_string())
                            .filter(|owner| !owner.is_empty()),
                    },
                ))
            }
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

fn normalize_source_env_bindings(
    bindings: impl IntoIterator<Item = EnvSourceEnvBinding>,
) -> Vec<EnvSourceEnvBinding> {
    bindings
        .into_iter()
        .filter_map(|binding| {
            let name = binding.name.trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some((
                    name.clone(),
                    EnvSourceEnvBinding {
                        name,
                        source_env_refs: normalize_names(binding.source_env_refs),
                    },
                ))
            }
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_materialization_plan_keeps_secret_values_out_of_handoff_metadata() {
        let env_plan = EnvMaterializationPlan {
            secret_refs: vec![EnvSecretRef {
                name: "API_TOKEN".to_string(),
                owner: Some("runner".to_string()),
            }],
            materialized_handoff: Some(EnvMaterializedHandoffMetadata {
                handoff_ref: "runner-artifact://run/env-handoff.json".to_string(),
                artifact_ref: Some("artifact://env-handoff".to_string()),
                env_names: vec!["API_TOKEN".to_string()],
            }),
            ..EnvMaterializationPlan::default()
        };

        let json = serde_json::to_string(&env_plan).expect("serializes env materialization plan");

        assert!(json.contains("API_TOKEN"));
        assert!(json.contains("runner-artifact://run/env-handoff.json"));
        assert!(!json.contains("super-secret-token-value"));
        assert!(!json.contains("value"));
    }
}
