use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use crate::core::defaults::{self, AgentTaskSecretSource};
use crate::core::keychain;
use crate::core::paths;
use crate::core::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

pub fn map_secret_to_env(
    name: &str,
    env_var: Option<&str>,
) -> crate::core::Result<AgentTaskSecretEnvStatus> {
    let mut config = AgentTaskSecretConfig::load();
    config.secrets.insert(
        name.to_string(),
        AgentTaskSecretSource {
            source: "env".to_string(),
            env_var: env_var.map(str::to_string),
            scope: None,
            name: None,
            field: None,
            value: None,
        },
    );
    config.save()?;
    Ok(secret_env_status_with_config(&[name.to_string()], &config)
        .into_iter()
        .next()
        .expect("single status"))
}

pub fn set_config_secret(name: &str, value: &str) -> crate::core::Result<AgentTaskSecretEnvStatus> {
    let mut config = AgentTaskSecretConfig::load();
    config.secrets.insert(
        name.to_string(),
        AgentTaskSecretSource {
            source: "config".to_string(),
            env_var: None,
            scope: None,
            name: None,
            field: None,
            value: Some(value.to_string()),
        },
    );
    config.save()?;
    Ok(secret_env_status_with_config(&[name.to_string()], &config)
        .into_iter()
        .next()
        .expect("single status"))
}

pub fn set_keychain_secret(
    name: &str,
    value: &str,
    scope: Option<&str>,
    keychain_name: Option<&str>,
) -> crate::core::Result<AgentTaskSecretEnvStatus> {
    let scope = scope.unwrap_or("agent-task");
    let keychain_name = keychain_name.unwrap_or(name);
    keychain::set(scope, keychain_name, value)?;

    let mut config = AgentTaskSecretConfig::load();
    config.secrets.insert(
        name.to_string(),
        AgentTaskSecretSource {
            source: "keychain".to_string(),
            env_var: None,
            scope: Some(scope.to_string()),
            name: Some(keychain_name.to_string()),
            field: None,
            value: None,
        },
    );
    config.save()?;
    Ok(secret_env_status_with_config(&[name.to_string()], &config)
        .into_iter()
        .next()
        .expect("single status"))
}

pub fn set_keychain_bundle(
    bundle: &str,
    value: &str,
    scope: Option<&str>,
    keychain_name: Option<&str>,
) -> crate::core::Result<String> {
    let _: Value = serde_json::from_str(value).map_err(|error| {
        Error::validation_invalid_argument(
            "value",
            format!("agent-task keychain bundle value must be JSON: {error}"),
            None,
            None,
        )
    })?;
    let scope = scope.unwrap_or("agent-task");
    let keychain_name = keychain_name.unwrap_or(bundle);
    keychain::set(scope, keychain_name, value)?;
    Ok(keychain_name.to_string())
}

pub fn map_secret_to_keychain_bundle(
    name: &str,
    bundle: &str,
    field: &str,
    scope: Option<&str>,
    keychain_name: Option<&str>,
) -> crate::core::Result<AgentTaskSecretEnvStatus> {
    let mut config = AgentTaskSecretConfig::load();
    config.secrets.insert(
        name.to_string(),
        AgentTaskSecretSource {
            source: "keychain-bundle".to_string(),
            env_var: None,
            scope: Some(scope.unwrap_or("agent-task").to_string()),
            name: Some(keychain_name.unwrap_or(bundle).to_string()),
            field: Some(field.to_string()),
            value: None,
        },
    );
    config.save()?;
    Ok(AgentTaskSecretEnvStatus {
        name: name.to_string(),
        configured: true,
        source: "keychain-bundle".to_string(),
    })
}

pub fn remove_secret_mapping(
    name: &str,
    remove_keychain: bool,
) -> crate::core::Result<AgentTaskSecretEnvStatus> {
    let mut config = AgentTaskSecretConfig::load();
    let source = config.secrets.remove(name);
    config.save()?;

    if remove_keychain {
        if let Some(source) = source.filter(|source| source.source == "keychain") {
            keychain::remove(
                source.scope.as_deref().unwrap_or("agent-task"),
                source.name.as_deref().unwrap_or(name),
            )?;
        }
    }

    Ok(secret_env_status_with_config(&[name.to_string()], &config)
        .into_iter()
        .next()
        .expect("single status"))
}

pub fn validate_secret_env(names: &[String]) -> Result<(), AgentTaskSecretResolutionError> {
    resolve_secret_env(names).map(|_| ())
}

fn secret_env_status_with_config(
    names: &[String],
    config: &AgentTaskSecretConfig,
) -> Vec<AgentTaskSecretEnvStatus> {
    let mut bundle_cache = HashMap::new();
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
                configured: source
                    .and_then(|source| source.resolve(name, &mut bundle_cache))
                    .is_some(),
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
    let mut bundle_cache = HashMap::new();

    for name in names {
        if let Ok(value) = env::var(name) {
            resolved.push((name.clone(), value));
            continue;
        }

        match config
            .secrets
            .get(name)
            .and_then(|source| source.resolve(name, &mut bundle_cache))
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
        let config = defaults::load_config();
        if !config.agent_task.secrets.is_empty() {
            return Self {
                secrets: config.agent_task.secrets,
            };
        }

        let Ok(path) = Self::path() else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn path() -> crate::core::Result<PathBuf> {
        paths::homeboy().map(|root| root.join("agent-task-secrets.json"))
    }

    fn save(&self) -> crate::core::Result<()> {
        let mut config = defaults::load_config();
        config.agent_task.secrets = self.secrets.clone();
        defaults::save_config(&config)?;
        Ok(())
    }
}

impl AgentTaskSecretSource {
    fn resolve(
        &self,
        requested_name: &str,
        bundle_cache: &mut HashMap<String, Option<Value>>,
    ) -> Option<String> {
        match self.source.as_str() {
            "config" => self.value.clone(),
            "env" => env::var(self.env_var.as_deref().unwrap_or(requested_name)).ok(),
            "keychain" => keychain::get(
                self.scope.as_deref().unwrap_or("agent-task"),
                self.name.as_deref().unwrap_or(requested_name),
            )
            .ok()
            .flatten(),
            "keychain-bundle" => self.resolve_keychain_bundle(requested_name, bundle_cache),
            _ => None,
        }
    }

    fn resolve_keychain_bundle(
        &self,
        requested_name: &str,
        bundle_cache: &mut HashMap<String, Option<Value>>,
    ) -> Option<String> {
        let scope = self.scope.as_deref().unwrap_or("agent-task");
        let keychain_name = self.name.as_deref().unwrap_or(requested_name);
        let cache_key = format!("{scope}\0{keychain_name}");
        let bundle = bundle_cache
            .entry(cache_key)
            .or_insert_with(|| {
                keychain::get(scope, keychain_name)
                    .ok()
                    .flatten()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            })
            .as_ref()?;
        let field = self.field.as_deref().unwrap_or(requested_name);
        bundle_field_value(bundle, field)
    }
}

fn bundle_field_value(bundle: &Value, field: &str) -> Option<String> {
    let mut value = bundle;
    for part in field.split('.') {
        value = value.get(part)?;
    }
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
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
                field: None,
                value: None,
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

    #[test]
    fn stores_and_resolves_declared_secret_from_global_config() {
        crate::test_support::with_isolated_home(|_| {
            let configured_name = unique_env("CONFIG_SECRET");
            std::env::remove_var(&configured_name);

            let status = set_config_secret(&configured_name, "stored-secret-value")
                .expect("secret config saved");

            assert_eq!(status.name, configured_name);
            assert!(status.configured);
            assert_eq!(status.source, "config");

            let resolved = resolve_secret_env(std::slice::from_ref(&status.name))
                .expect("config secret resolves");
            assert_eq!(
                resolved,
                vec![(status.name, "stored-secret-value".to_string())]
            );
        });
    }

    #[test]
    fn maps_declared_secret_to_env_source_file() {
        crate::test_support::with_isolated_home(|_| {
            let configured_name = unique_env("MAPPED_SECRET");
            let source_name = unique_env("MAPPED_SOURCE");
            std::env::remove_var(&configured_name);
            std::env::set_var(&source_name, "mapped-secret-value");

            let status = map_secret_to_env(&configured_name, Some(&source_name))
                .expect("secret mapping saved");

            assert_eq!(status.name, configured_name);
            assert!(status.configured);
            assert_eq!(status.source, "env");

            let resolved = resolve_secret_env(std::slice::from_ref(&status.name))
                .expect("mapped secret resolves");
            assert_eq!(
                resolved,
                vec![(status.name, "mapped-secret-value".to_string())]
            );
            std::env::remove_var(source_name);
        });
    }

    #[test]
    fn removes_declared_secret_mapping() {
        crate::test_support::with_isolated_home(|_| {
            let configured_name = unique_env("REMOVED_SECRET");
            let source_name = unique_env("REMOVED_SOURCE");
            std::env::set_var(&source_name, "removed-secret-value");
            map_secret_to_env(&configured_name, Some(&source_name)).expect("secret mapping saved");

            let status = remove_secret_mapping(&configured_name, false).expect("mapping removed");

            assert_eq!(status.name, configured_name);
            assert!(!status.configured);
            assert_eq!(status.source, "missing");
            std::env::remove_var(source_name);
        });
    }

    #[test]
    fn resolves_keychain_bundle_fields_from_cached_bundle() {
        let source = AgentTaskSecretSource {
            source: "keychain-bundle".to_string(),
            env_var: None,
            scope: Some("agent-task".to_string()),
            name: Some("provider-oauth".to_string()),
            field: Some("tokens.access".to_string()),
            value: None,
        };
        let mut cache = HashMap::new();
        cache.insert(
            "agent-task\0provider-oauth".to_string(),
            Some(serde_json::json!({
                "tokens": {
                    "access": "access-token",
                    "expires": 12345,
                    "fedramp": false
                }
            })),
        );

        assert_eq!(
            source.resolve("PROVIDER_ACCESS_TOKEN", &mut cache),
            Some("access-token".to_string())
        );

        let numeric_source = AgentTaskSecretSource {
            field: Some("tokens.expires".to_string()),
            ..source.clone()
        };
        assert_eq!(
            numeric_source.resolve("PROVIDER_EXPIRES_AT", &mut cache),
            Some("12345".to_string())
        );

        let bool_source = AgentTaskSecretSource {
            field: Some("tokens.fedramp".to_string()),
            ..source
        };
        assert_eq!(
            bool_source.resolve("PROVIDER_FEDRAMP", &mut cache),
            Some("false".to_string())
        );
    }

    #[test]
    fn maps_secret_to_keychain_bundle_without_reading_keychain() {
        crate::test_support::with_isolated_home(|_| {
            let status = map_secret_to_keychain_bundle(
                "PROVIDER_ACCESS_TOKEN",
                "provider-oauth",
                "tokens.access",
                None,
                None,
            )
            .expect("bundle mapping saved");

            assert_eq!(status.name, "PROVIDER_ACCESS_TOKEN");
            assert!(status.configured);
            assert_eq!(status.source, "keychain-bundle");

            let config = AgentTaskSecretConfig::load();
            let source = config
                .secrets
                .get("PROVIDER_ACCESS_TOKEN")
                .expect("mapping stored");
            assert_eq!(source.source, "keychain-bundle");
            assert_eq!(source.name.as_deref(), Some("provider-oauth"));
            assert_eq!(source.field.as_deref(), Some("tokens.access"));
        });
    }
}
