use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::core::redaction::RedactionPolicy;

pub const SECRET_ENV_PLAN_SCHEMA: &str = "homeboy/secret-env-plan/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlan {
    #[serde(default = "secret_env_plan_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub public_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_credentials: BTreeMap<String, SecretEnvProviderCredentialMapping>,
    #[serde(default, skip_serializing_if = "SecretEnvRedactionPolicy::is_default")]
    pub redaction: SecretEnvRedactionPolicy,
}

impl Default for SecretEnvPlan {
    fn default() -> Self {
        Self {
            schema: SECRET_ENV_PLAN_SCHEMA.to_string(),
            public_env: BTreeMap::new(),
            secret_env_names: Vec::new(),
            provider_credentials: BTreeMap::new(),
            redaction: SecretEnvRedactionPolicy::default(),
        }
    }
}

impl SecretEnvPlan {
    pub fn from_secret_env_names(names: impl IntoIterator<Item = String>) -> Self {
        let mut plan = Self::default();
        plan.secret_env_names = normalize_names(names);
        plan
    }

    pub fn secret_env_names(&self) -> Vec<String> {
        let mapped_names = self
            .provider_credentials
            .values()
            .flat_map(|mapping| mapping.secret_env.iter().cloned());
        normalize_names(self.secret_env_names.iter().cloned().chain(mapped_names))
    }

    pub fn materialize(
        &self,
        secret_values: impl IntoIterator<Item = (String, String)>,
    ) -> BTreeMap<String, String> {
        let mut env = self.public_env.clone();
        for (name, value) in secret_values {
            env.insert(name, value);
        }
        env
    }

    pub fn redacted_env(&self) -> BTreeMap<String, String> {
        let policy = self.redaction.to_policy();
        let mut env = self
            .public_env
            .iter()
            .map(|(name, value)| (name.clone(), redact_env_value(&policy, value)))
            .collect::<BTreeMap<_, _>>();
        for name in self.secret_env_names() {
            env.insert(name, self.redaction.replacement.clone());
        }
        env
    }

    pub fn redacted(&self) -> Self {
        let policy = self.redaction.to_policy();
        let mut redacted = self.clone();
        redacted.public_env = self
            .public_env
            .iter()
            .map(|(name, value)| (name.clone(), redact_env_value(&policy, value)))
            .collect();
        redacted
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvProviderCredentialMapping {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, SecretEnvCredentialSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvCredentialSource {
    #[serde(default = "default_secret_env_source")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRedactionPolicy {
    #[serde(default = "default_replacement")]
    pub replacement: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive_env_names: Vec<String>,
}

impl Default for SecretEnvRedactionPolicy {
    fn default() -> Self {
        Self {
            replacement: default_replacement(),
            sensitive_env_names: Vec::new(),
        }
    }
}

impl SecretEnvRedactionPolicy {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }

    fn to_policy(&self) -> RedactionPolicy {
        self.sensitive_env_names.iter().fold(
            RedactionPolicy::default().with_replacement(self.replacement.clone()),
            |policy, name| policy.with_sensitive_key(name),
        )
    }
}

fn normalize_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn redact_env_value(policy: &RedactionPolicy, value: &str) -> String {
    if value.contains("://") || value.starts_with('/') && value.contains('?') {
        policy.redact_url(value)
    } else {
        policy.redact_string(value)
    }
}

fn secret_env_plan_schema() -> String {
    SECRET_ENV_PLAN_SCHEMA.to_string()
}

fn default_secret_env_source() -> String {
    "env".to_string()
}

fn default_replacement() -> String {
    "[REDACTED]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_env_plan_materializes_public_and_secret_values() {
        let plan = SecretEnvPlan {
            public_env: BTreeMap::from([("PUBLIC_FLAG".to_string(), "1".to_string())]),
            secret_env_names: vec!["API_TOKEN".to_string()],
            ..SecretEnvPlan::default()
        };

        let env = plan.materialize([("API_TOKEN".to_string(), "secret-value".to_string())]);

        assert_eq!(env.get("PUBLIC_FLAG"), Some(&"1".to_string()));
        assert_eq!(env.get("API_TOKEN"), Some(&"secret-value".to_string()));
    }

    #[test]
    fn secret_env_plan_redacts_secret_values_without_storing_them() {
        let mut provider_credentials = BTreeMap::new();
        provider_credentials.insert(
            "codex".to_string(),
            SecretEnvProviderCredentialMapping {
                secret_env: vec!["CODEX_REFRESH_TOKEN".to_string()],
                sources: BTreeMap::from([(
                    "CODEX_REFRESH_TOKEN".to_string(),
                    SecretEnvCredentialSource {
                        source: "keychain-bundle".to_string(),
                        scope: Some("agent-task".to_string()),
                        name: Some("codex-oauth".to_string()),
                        field: Some("refresh_token".to_string()),
                        env_var: None,
                    },
                )]),
            },
        );
        let plan = SecretEnvPlan {
            public_env: BTreeMap::from([(
                "PUBLIC_ENDPOINT".to_string(),
                "https://example.test/?token=abc&ok=1".to_string(),
            )]),
            provider_credentials,
            ..SecretEnvPlan::default()
        };

        let serialized = serde_json::to_string(&plan).expect("plan json");
        let redacted = plan.redacted_env();

        assert!(!serialized.contains("secret-value"));
        assert_eq!(
            redacted.get("PUBLIC_ENDPOINT"),
            Some(&"https://example.test/?token=[REDACTED]&ok=1".to_string())
        );
        assert_eq!(
            redacted.get("CODEX_REFRESH_TOKEN"),
            Some(&"[REDACTED]".to_string())
        );
    }

    #[test]
    fn secret_env_plan_deduplicates_direct_and_provider_secret_names() {
        let plan = SecretEnvPlan {
            secret_env_names: vec!["B_SECRET".to_string(), "A_SECRET".to_string()],
            provider_credentials: BTreeMap::from([(
                "provider".to_string(),
                SecretEnvProviderCredentialMapping {
                    secret_env: vec!["A_SECRET".to_string(), "C_SECRET".to_string()],
                    sources: BTreeMap::new(),
                },
            )]),
            ..SecretEnvPlan::default()
        };

        assert_eq!(
            plan.secret_env_names(),
            vec![
                "A_SECRET".to_string(),
                "B_SECRET".to_string(),
                "C_SECRET".to_string()
            ]
        );
    }
}
