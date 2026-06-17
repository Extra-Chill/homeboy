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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvStatus {
    pub name: String,
    pub configured: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvResolution {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvResolutionError {
    pub missing_secret_env: Vec<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
}

pub struct SecretEnvValueProvider<'a> {
    source: String,
    resolve: Box<dyn FnMut(&str) -> Option<String> + 'a>,
}

impl<'a> SecretEnvValueProvider<'a> {
    pub fn new(
        source: impl Into<String>,
        resolve: impl FnMut(&str) -> Option<String> + 'a,
    ) -> Self {
        Self {
            source: source.into(),
            resolve: Box::new(resolve),
        }
    }
}

pub fn resolve_secret_env_names(
    names: impl IntoIterator<Item = String>,
    providers: Vec<SecretEnvValueProvider<'_>>,
    missing_message_prefix: &str,
) -> Result<SecretEnvResolution, SecretEnvResolutionError> {
    SecretEnvResolver::new(providers).resolve_required(names, missing_message_prefix)
}

pub struct SecretEnvResolver<'a> {
    providers: Vec<SecretEnvValueProvider<'a>>,
}

impl<'a> SecretEnvResolver<'a> {
    pub fn new(providers: Vec<SecretEnvValueProvider<'a>>) -> Self {
        Self { providers }
    }

    pub fn resolve_required(
        mut self,
        names: impl IntoIterator<Item = String>,
        missing_message_prefix: &str,
    ) -> Result<SecretEnvResolution, SecretEnvResolutionError> {
        let names = normalize_names(names);
        let mut env = Vec::new();
        let mut status = Vec::new();
        let mut missing = Vec::new();

        for name in names {
            let mut resolved = None;
            for provider in self.providers.iter_mut() {
                if let Some(value) = (provider.resolve)(&name) {
                    resolved = Some((provider.source.clone(), value));
                    break;
                }
            }

            if let Some((source, value)) = resolved {
                env.push((name.clone(), value));
                status.push(secret_env_status(name, true, source));
            } else {
                status.push(secret_env_status(name.clone(), false, "missing"));
                missing.push(name);
            }
        }

        if missing.is_empty() {
            Ok(SecretEnvResolution { env, status })
        } else {
            Err(SecretEnvResolutionError {
                message: format!("{missing_message_prefix}: {}", missing.join(", ")),
                missing_secret_env: missing,
                status,
            })
        }
    }
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

pub fn normalize_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn secret_env_status(name: String, configured: bool, source: impl Into<String>) -> SecretEnvStatus {
    SecretEnvStatus {
        name,
        configured,
        source: source.into(),
    }
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

    #[test]
    fn normalizes_secret_env_names_by_trimming_sorting_and_deduplicating() {
        assert_eq!(
            normalize_names([
                " B_SECRET ".to_string(),
                "".to_string(),
                "A_SECRET".to_string(),
                "B_SECRET".to_string(),
            ]),
            vec!["A_SECRET".to_string(), "B_SECRET".to_string()]
        );
    }

    #[test]
    fn resolver_returns_values_and_redacted_status_contract() {
        let resolver = SecretEnvResolver::new(vec![
            SecretEnvValueProvider::new("missing-source", |_| None),
            SecretEnvValueProvider::new("test-source", |name| {
                (name == "API_TOKEN").then(|| "secret-value".to_string())
            }),
        ]);

        let resolved = resolver
            .resolve_required(vec!["API_TOKEN".to_string()], "missing required secret env")
            .expect("secret resolves");

        assert_eq!(
            resolved.env,
            vec![("API_TOKEN".to_string(), "secret-value".to_string())]
        );
        assert_eq!(
            resolved.status,
            vec![SecretEnvStatus {
                name: "API_TOKEN".to_string(),
                configured: true,
                source: "test-source".to_string(),
            }]
        );
        assert!(!serde_json::to_string(&resolved.status)
            .expect("status json")
            .contains("secret-value"));
    }

    #[test]
    fn resolver_reports_missing_names_without_values() {
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-source", |_| None)]);

        let error = resolver
            .resolve_required(
                vec!["B_SECRET".to_string(), "A_SECRET".to_string()],
                "missing required secret env",
            )
            .expect_err("secrets are missing");

        assert_eq!(
            error.missing_secret_env,
            vec!["A_SECRET".to_string(), "B_SECRET".to_string()]
        );
        assert_eq!(
            error.message,
            "missing required secret env: A_SECRET, B_SECRET"
        );
        assert!(!serde_json::to_string(&error)
            .expect("error json")
            .contains("secret-value"));
    }
}
