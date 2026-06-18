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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<SecretEnvRequirement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_credentials: BTreeMap<String, SecretEnvProviderCredentialMapping>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_name_mapping: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
    #[serde(
        default,
        skip_serializing_if = "SecretEnvRedactionPolicy::is_default_policy"
    )]
    pub redaction: SecretEnvRedactionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRequirement {
    pub name: String,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<SecretEnvRefreshHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRefreshHint {
    pub provider: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlanDiagnostics {
    pub passthrough_secret_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvDiagnosticStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvDiagnosticStatus {
    pub name: String,
    pub required: bool,
    pub configured: bool,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<SecretEnvRefreshHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlanDiagnosticError {
    pub missing_required_secret_env: Vec<String>,
    pub message: String,
    pub diagnostics: SecretEnvPlanDiagnostics,
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
            requirements: Vec::new(),
            provider_credentials: BTreeMap::new(),
            env_name_mapping: BTreeMap::new(),
            status: Vec::new(),
            redaction: SecretEnvRedactionPolicy::default(),
        }
    }
}

impl SecretEnvPlan {
    pub fn from_secret_env_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            secret_env_names: normalize_names(names),
            ..Self::default()
        }
    }

    pub fn from_requirements(requirements: impl IntoIterator<Item = SecretEnvRequirement>) -> Self {
        let mut plan = Self::default();
        plan.requirements = normalize_requirements(requirements);
        plan.secret_env_names = plan
            .requirements
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect();
        plan
    }

    pub fn secret_env_names(&self) -> Vec<String> {
        let mapped_names = self
            .provider_credentials
            .values()
            .flat_map(|mapping| mapping.secret_env.iter().cloned());
        let env_name_mapping_names = self
            .env_name_mapping
            .values()
            .flat_map(|names| names.iter().cloned());
        normalize_names(
            self.secret_env_names
                .iter()
                .cloned()
                .chain(
                    self.requirements
                        .iter()
                        .map(|requirement| requirement.name.clone()),
                )
                .chain(mapped_names)
                .chain(env_name_mapping_names),
        )
    }

    pub fn diagnose(
        &self,
        status: impl IntoIterator<Item = SecretEnvStatus>,
    ) -> Result<SecretEnvPlanDiagnostics, SecretEnvPlanDiagnosticError> {
        let status_by_name = status
            .into_iter()
            .map(|status| (status.name.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let requirements = self.secret_env_requirements();
        let passthrough_secret_env_names = requirements
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect::<Vec<_>>();
        let mut missing_required_secret_env = Vec::new();
        let mut diagnostic_status = Vec::new();

        for requirement in requirements {
            let status = status_by_name
                .get(&requirement.name)
                .cloned()
                .unwrap_or_else(|| secret_env_status(requirement.name.clone(), false, "missing"));
            if requirement.required && !status.configured {
                missing_required_secret_env.push(requirement.name.clone());
            }
            diagnostic_status.push(SecretEnvDiagnosticStatus {
                name: requirement.name,
                required: requirement.required,
                configured: status.configured,
                source: status.source,
                refresh: requirement.refresh,
            });
        }

        let diagnostics = SecretEnvPlanDiagnostics {
            passthrough_secret_env_names,
            status: diagnostic_status,
        };

        if missing_required_secret_env.is_empty() {
            Ok(diagnostics)
        } else {
            Err(SecretEnvPlanDiagnosticError {
                message: format!(
                    "missing required secret env: {}",
                    missing_required_secret_env.join(", ")
                ),
                missing_required_secret_env,
                diagnostics,
            })
        }
    }

    fn secret_env_requirements(&self) -> Vec<SecretEnvRequirement> {
        let mut requirements = self
            .requirements
            .iter()
            .cloned()
            .map(|mut requirement| {
                requirement.name = requirement.name.trim().to_string();
                requirement
            })
            .filter(|requirement| !requirement.name.is_empty())
            .map(|requirement| (requirement.name.clone(), requirement))
            .collect::<BTreeMap<_, _>>();

        for name in self.secret_env_names() {
            requirements
                .entry(name.clone())
                .or_insert_with(|| SecretEnvRequirement {
                    name,
                    required: true,
                    refresh: None,
                });
        }

        requirements.into_values().collect()
    }

    pub fn extend_secret_env_names(&mut self, names: impl IntoIterator<Item = String>) {
        self.secret_env_names = normalize_names(self.secret_env_names.iter().cloned().chain(names));
    }

    pub fn map_env_names(
        &mut self,
        key: impl Into<String>,
        names: impl IntoIterator<Item = String>,
    ) {
        let names = normalize_names(names);
        if !names.is_empty() {
            self.env_name_mapping.insert(key.into(), names);
        }
    }

    pub fn with_status(mut self, status: impl IntoIterator<Item = SecretEnvStatus>) -> Self {
        self.status = status.into_iter().collect();
        self
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
    fn is_default_policy(&self) -> bool {
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

fn normalize_requirements(
    requirements: impl IntoIterator<Item = SecretEnvRequirement>,
) -> Vec<SecretEnvRequirement> {
    requirements
        .into_iter()
        .map(|mut requirement| {
            requirement.name = requirement.name.trim().to_string();
            requirement
        })
        .filter(|requirement| !requirement.name.is_empty())
        .map(|requirement| (requirement.name.clone(), requirement))
        .collect::<BTreeMap<_, _>>()
        .into_values()
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

fn default_required() -> bool {
    true
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
    fn secret_env_plan_includes_mapped_env_names() {
        let mut plan = SecretEnvPlan::from_secret_env_names(["DIRECT_SECRET".to_string()]);
        plan.map_env_names(
            "provider.example",
            ["MAPPED_SECRET".to_string(), "DIRECT_SECRET".to_string()],
        );

        assert_eq!(
            plan.secret_env_names(),
            vec!["DIRECT_SECRET".to_string(), "MAPPED_SECRET".to_string()]
        );
        assert_eq!(
            plan.env_name_mapping.get("provider.example"),
            Some(&vec![
                "DIRECT_SECRET".to_string(),
                "MAPPED_SECRET".to_string()
            ])
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

    #[test]
    fn diagnostics_fail_for_missing_required_secret_env() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "PROVIDER_TOKEN".to_string(),
            required: true,
            refresh: None,
        }]);

        let error = plan
            .diagnose([secret_env_status(
                "PROVIDER_TOKEN".to_string(),
                false,
                "missing",
            )])
            .expect_err("required secret env is missing");

        assert_eq!(
            error.missing_required_secret_env,
            vec!["PROVIDER_TOKEN".to_string()]
        );
        assert_eq!(
            error.diagnostics.passthrough_secret_env_names,
            vec!["PROVIDER_TOKEN".to_string()]
        );
    }

    #[test]
    fn diagnostics_allow_missing_optional_secret_env() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "OPTIONAL_PROVIDER_TOKEN".to_string(),
            required: false,
            refresh: None,
        }]);

        let diagnostics = plan
            .diagnose([])
            .expect("optional secret env can be absent");

        assert_eq!(
            diagnostics.passthrough_secret_env_names,
            vec!["OPTIONAL_PROVIDER_TOKEN".to_string()]
        );
        assert_eq!(
            diagnostics.status,
            vec![SecretEnvDiagnosticStatus {
                name: "OPTIONAL_PROVIDER_TOKEN".to_string(),
                required: false,
                configured: false,
                source: "missing".to_string(),
                refresh: None,
            }]
        );
    }

    #[test]
    fn diagnostics_are_redacted_and_expose_passthrough_names_only() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
            required: true,
            refresh: None,
        }]);

        let diagnostics = plan
            .diagnose([secret_env_status(
                "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
                true,
                "test-source",
            )])
            .expect("required secret env is configured");
        let serialized = serde_json::to_string(&diagnostics).expect("diagnostics json");

        assert_eq!(
            diagnostics.passthrough_secret_env_names,
            vec!["FIXTURE_PROVIDER_ACCESS_TOKEN".to_string()]
        );
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains("access-token-value"));
    }

    #[test]
    fn diagnostics_preserve_provider_refresh_metadata() {
        let refresh = SecretEnvRefreshHint {
            provider: "fixture-provider".to_string(),
            metadata: BTreeMap::from([
                ("credential_kind".to_string(), "oauth".to_string()),
                (
                    "refresh_env".to_string(),
                    "FIXTURE_PROVIDER_REFRESH_TOKEN".to_string(),
                ),
            ]),
        };
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
            required: true,
            refresh: Some(refresh.clone()),
        }]);

        let diagnostics = plan
            .diagnose([secret_env_status(
                "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
                false,
                "missing",
            )])
            .expect_err("missing required secret env")
            .diagnostics;

        assert_eq!(diagnostics.status[0].refresh, Some(refresh));
    }
}
