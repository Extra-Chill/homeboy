use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::keychain;
use crate::core::paths;
use crate::core::Error;

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
        },
    );
    config.save()?;
    Ok(secret_env_status_with_config(&[name.to_string()], &config)
        .into_iter()
        .next()
        .expect("single status"))
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

pub fn map_claude_code_to_opencode_anthropic() -> crate::core::Result<Vec<AgentTaskSecretEnvStatus>>
{
    let mut config = AgentTaskSecretConfig::load();
    for (env_name, field_name) in [
        ("AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN", "access"),
        ("AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN", "refresh"),
        ("AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT", "expires"),
    ] {
        config.secrets.insert(
            env_name.to_string(),
            AgentTaskSecretSource {
                source: "opencode-anthropic-auth".to_string(),
                env_var: None,
                scope: None,
                name: Some(field_name.to_string()),
            },
        );
    }
    config.save()?;
    Ok(secret_env_status_with_config(
        &[
            "AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN".to_string(),
            "AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT".to_string(),
        ],
        &config,
    ))
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
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "create agent-task secret config dir {}",
                        parent.display()
                    )),
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(self).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize agent-task secret config".to_string()),
            )
        })?;
        fs::write(&path, format!("{raw}\n")).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write agent-task secret config {}", path.display())),
            )
        })?;
        Ok(())
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
            "opencode-anthropic-auth" => {
                opencode_anthropic_auth_value(self.name.as_deref().unwrap_or(requested_name))
            }
            _ => None,
        }
    }
}

fn opencode_anthropic_auth_value(field_name: &str) -> Option<String> {
    let path = opencode_auth_file_path()?;
    let raw = fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&raw).ok()?;
    let auth = data.get("anthropic")?;
    if auth.get("type").and_then(Value::as_str) != Some("oauth") {
        return None;
    }

    match field_name {
        "access" | "refresh" => auth
            .get(field_name)
            .and_then(Value::as_str)
            .map(str::to_string),
        "expires" => auth.get("expires").and_then(|value| {
            value
                .as_i64()
                .map(|number| number.to_string())
                .or_else(|| value.as_str().map(str::to_string))
        }),
        _ => None,
    }
}

fn opencode_auth_file_path() -> Option<PathBuf> {
    if let Ok(data_home) = env::var("XDG_DATA_HOME") {
        return Some(PathBuf::from(data_home).join("opencode/auth.json"));
    }
    env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".local/share/opencode/auth.json"))
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
    fn maps_claude_code_to_opencode_anthropic_auth_file() {
        crate::test_support::with_isolated_home(|home| {
            let auth_path = home.path().join(".local/share/opencode/auth.json");
            std::fs::create_dir_all(auth_path.parent().expect("auth parent")).expect("mkdir auth");
            std::fs::write(
                &auth_path,
                r#"{"anthropic":{"type":"oauth","access":"access-token","refresh":"refresh-token","expires":12345}}"#,
            )
            .expect("write auth");

            let status = map_claude_code_to_opencode_anthropic().expect("mapping saved");

            assert_eq!(status.len(), 3);
            assert!(status.iter().all(|status| status.configured));
            assert!(status
                .iter()
                .all(|status| status.source == "opencode-anthropic-auth"));

            let resolved = resolve_secret_env(&[
                "AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN".to_string(),
                "AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN".to_string(),
                "AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT".to_string(),
            ])
            .expect("mapped auth resolves");

            assert_eq!(
                resolved,
                vec![
                    (
                        "AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN".to_string(),
                        "access-token".to_string()
                    ),
                    (
                        "AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN".to_string(),
                        "refresh-token".to_string()
                    ),
                    (
                        "AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT".to_string(),
                        "12345".to_string()
                    ),
                ]
            );
        });
    }
}
