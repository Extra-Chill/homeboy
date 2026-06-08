use std::collections::HashMap;
use std::env;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::core::keychain;
use crate::core::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskSecretResolutionError {
    pub missing_secret_env: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskSecretEnvStatus {
    pub name: String,
    pub configured: bool,
    pub source: String,
}

pub fn resolve_secret_env(
    names: &[String],
) -> Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    resolve_secret_env_with_config(names, &AgentTaskSecretConfig::load())
}

pub fn secret_env_status(names: &[String]) -> Vec<AgentTaskSecretEnvStatus> {
    secret_env_status_with_config(names, &AgentTaskSecretConfig::load())
}

pub fn validate_secret_env(names: &[String]) -> Result<(), AgentTaskSecretResolutionError> {
    resolve_secret_env(names).map(|_| ())
}

fn secret_env_status_with_config(
    names: &[String],
    config: &AgentTaskSecretConfig,
) -> Vec<AgentTaskSecretEnvStatus> {
    names
        .iter()
        .map(|name| {
            if env::var(name).is_ok() {
                return AgentTaskSecretEnvStatus {
                    name: name.clone(),
                    configured: true,
                    source: "env".to_string(),
                };
            }

            let source = config.secrets.get(name);
            AgentTaskSecretEnvStatus {
                name: name.clone(),
                configured: source.is_some_and(|source| source.resolve(name).is_some()),
                source: source
                    .map(|source| source.source.clone())
                    .unwrap_or_else(|| "missing".to_string()),
            }
        })
        .collect()
}

fn resolve_secret_env_with_config(
    names: &[String],
    config: &AgentTaskSecretConfig,
) -> Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }

    let mut resolved = Vec::new();
    let mut missing = Vec::new();

    for name in names {
        if let Ok(value) = env::var(name) {
            resolved.push((name.clone(), value));
            continue;
        }

        match config
            .secrets
            .get(name)
            .and_then(|source| source.resolve(name))
        {
            Some(value) => resolved.push((name.clone(), value)),
            None => missing.push(name.clone()),
        }
    }

    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(AgentTaskSecretResolutionError {
            message: format!(
                "missing required agent-task secret env: {}",
                missing.join(", ")
            ),
            missing_secret_env: missing,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTaskSecretConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    secrets: HashMap<String, AgentTaskSecretSource>,
}

impl AgentTaskSecretConfig {
    fn load() -> Self {
        let Ok(path) = paths::homeboy().map(|root| root.join("agent-task-secrets.json")) else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTaskSecretSource {
    #[serde(default = "default_source")]
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl AgentTaskSecretSource {
    fn resolve(&self, requested_name: &str) -> Option<String> {
        match self.source.as_str() {
            "env" => env::var(self.env_var.as_deref().unwrap_or(requested_name)).ok(),
            "keychain" => keychain::get(
                self.scope.as_deref().unwrap_or("agent-task"),
                self.name.as_deref().unwrap_or(requested_name),
            )
            .ok()
            .flatten(),
            _ => None,
        }
    }
}

fn default_source() -> String {
    "env".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_env(name: &str) -> String {
        format!("HOMEBOY_TEST_{}_{}", name, std::process::id())
    }

    #[test]
    fn resolves_declared_secret_from_process_env() {
        let name = unique_env("DIRECT_SECRET");
        std::env::set_var(&name, "secret-value");

        let resolved = resolve_secret_env(std::slice::from_ref(&name)).expect("secret resolves");

        assert_eq!(resolved, vec![(name.clone(), "secret-value".to_string())]);
        std::env::remove_var(name);
    }

    #[test]
    fn reports_missing_declared_secret_names_without_values() {
        let name = unique_env("MISSING_SECRET");
        std::env::remove_var(&name);

        let error = resolve_secret_env(std::slice::from_ref(&name)).expect_err("secret missing");

        assert_eq!(error.missing_secret_env, vec![name.clone()]);
        assert!(error.message.contains(&name));
        assert!(!error.message.contains("secret-value"));
    }

    #[test]
    fn reports_redacted_secret_readiness() {
        let present = unique_env("READY_SECRET");
        let missing = unique_env("NOT_READY_SECRET");
        std::env::set_var(&present, "secret-value");
        std::env::remove_var(&missing);
        let names = vec![present.clone(), missing.clone()];

        let status = secret_env_status_with_config(&names, &AgentTaskSecretConfig::default());

        assert_eq!(status[0].name, present);
        assert!(status[0].configured);
        assert_eq!(status[0].source, "env");
        assert_eq!(status[1].name, missing);
        assert!(!status[1].configured);
        assert_eq!(status[1].source, "missing");
        let serialized = serde_json::to_string(&status).expect("status json");
        assert!(!serialized.contains("secret-value"));
        std::env::remove_var(present);
    }

    #[test]
    fn resolves_declared_secret_from_configured_env_source() {
        let configured_name = unique_env("CONFIGURED_SECRET");
        let source_name = unique_env("CONFIGURED_SOURCE");
        std::env::remove_var(&configured_name);
        std::env::set_var(&source_name, "configured-secret-value");
        let mut secrets = HashMap::new();
        secrets.insert(
            configured_name.clone(),
            AgentTaskSecretSource {
                source: "env".to_string(),
                env_var: Some(source_name.clone()),
                scope: None,
                name: None,
            },
        );
        let config = AgentTaskSecretConfig { secrets };

        let resolved =
            resolve_secret_env_with_config(std::slice::from_ref(&configured_name), &config)
                .expect("secret resolves");

        assert_eq!(
            resolved,
            vec![(configured_name, "configured-secret-value".to_string())]
        );
        std::env::remove_var(source_name);
    }
}
