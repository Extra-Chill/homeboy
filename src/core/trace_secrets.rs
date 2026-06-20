use serde::Serialize;

use crate::core::{agent_task_secrets, keychain, Error, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceSecretEnvStatus {
    pub name: String,
    pub configured: bool,
    pub source: String,
}

pub fn resolve_secret_env(
    names: &[String],
    project_id: Option<&str>,
) -> Result<(Vec<(String, String)>, Vec<TraceSecretEnvStatus>)> {
    let names = normalize_names(names);
    let mut resolved = Vec::new();
    let mut statuses = Vec::new();
    let mut missing = Vec::new();

    for name in names {
        if let Ok(value) = std::env::var(&name) {
            resolved.push((name.clone(), value));
            statuses.push(status(name, true, "env"));
            continue;
        }

        if let Ok(mut values) = agent_task_secrets::resolve_secret_env(std::slice::from_ref(&name))
        {
            if let Some((_resolved_name, value)) = values.pop() {
                resolved.push((name.clone(), value));
                statuses.push(status(name, true, "agent-task"));
                continue;
            }
        }

        if let Some(project_id) = project_id {
            match keychain::get(project_id, &name) {
                Ok(Some(value)) => {
                    resolved.push((name.clone(), value));
                    statuses.push(status(name, true, "project-keychain"));
                    continue;
                }
                Ok(None) => {}
                Err(error) if keychain::is_secure_storage_unavailable(&error) => {
                    // The OS keychain is reachable but cannot service the read
                    // without an interactive UI (macOS dark wake, locked login
                    // keychain, or headless/SSH session). Surface an actionable
                    // diagnostic that points at the env-first escape hatch
                    // instead of failing with an opaque platform error.
                    return Err(Error::validation_invalid_argument(
                        "secret-env",
                        format!(
                            "could not read trace secret '{name}' from the project keychain: {}",
                            error.message
                        ),
                        None,
                        Some(vec![
                            format!(
                                "Export the value in the controller shell before running trace, for example `export {name}=...`; `--secret-env` prefers process env over the keychain."
                            ),
                            "This happens when the OS keychain needs UI that isn't available (macOS dark wake, a locked login keychain, or a headless/SSH session). Rerun from an unlocked, interactive user session, or use exported env values.".to_string(),
                            "For repeatable headless runs, configure a `source: env` mapping with `homeboy agent-task auth map-env` so trace secrets resolve without keychain access.".to_string(),
                        ]),
                    ));
                }
                Err(error) => return Err(error),
            }
        }

        statuses.push(status(name.clone(), false, "missing"));
        missing.push(name);
    }

    if missing.is_empty() {
        Ok((resolved, statuses))
    } else {
        Err(Error::validation_invalid_argument(
            "secret-env",
            format!("missing required trace secret env: {}", missing.join(", ")),
            None,
            Some(vec![
                project_id
                    .map(|project_id| {
                        format!(
                            "Store project secrets with `homeboy auth set --project {project_id} <VARIABLE>`."
                        )
                    })
                    .unwrap_or_else(|| {
                        "Store project secrets with `homeboy auth set --project <project> <VARIABLE>`.".to_string()
                    }),
                "Or configure a reusable mapping with `homeboy agent-task auth map-env` / `homeboy agent-task auth set-keychain`, or export the variable before running trace.".to_string(),
            ]),
        ))
    }
}

pub fn empty_status() -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/trace-secret-env/v1",
        "secret_env": [],
    })
}

pub fn status_metadata(statuses: Vec<TraceSecretEnvStatus>) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/trace-secret-env/v1",
        "secret_env": statuses,
    })
}

fn normalize_names(names: &[String]) -> Vec<String> {
    let mut names = names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn status(name: String, configured: bool, source: &str) -> TraceSecretEnvStatus {
    TraceSecretEnvStatus {
        name,
        configured,
        source: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_metadata_does_not_include_values() {
        let metadata = status_metadata(vec![TraceSecretEnvStatus {
            name: "STRIPE_SECRET_KEY".to_string(),
            configured: true,
            source: "project-keychain".to_string(),
        }]);

        let rendered = metadata.to_string();
        assert!(rendered.contains("STRIPE_SECRET_KEY"));
        assert!(rendered.contains("project-keychain"));
        assert!(!rendered.contains("sk_test_fake_not_real"));
    }
}
