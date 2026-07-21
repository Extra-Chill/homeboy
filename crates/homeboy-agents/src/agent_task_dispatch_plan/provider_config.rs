//! Provider-config assembly and secret-env resolution for agent-task dispatch.
//!
//! Builds the executor `provider_config` object (merging materialized refs,
//! workspace metadata, and dispatch request inputs), promotes component
//! contracts into it, and derives the secret-env allowlist. Extracted from
//! `agent_task_dispatch_plan` to keep the plan builder focused on plan shape.

use serde_json::Value;

use super::prompt_spec::read_text_spec;
use super::DispatchWorkspaceTarget;
use crate::agent_task::AgentTaskComponentContract;
use crate::agent_task_config_materialization::materialize_provider_config_refs;
use crate::agent_task_dispatch_service::AgentTaskDispatchRequest;
use homeboy_core::{defaults, Error, Result};

/// Error returned when a resolved provider config is not a JSON object. Shared
/// by the explicit-spec and post-default validation paths so both surface an
/// identical message (#5091).
fn provider_config_must_be_object_error() -> Error {
    Error::validation_invalid_argument(
        "provider-config",
        "agent-task cook --provider-config must resolve to a JSON object",
        None,
        None,
    )
}

pub(crate) fn dispatch_provider_config(
    request: &AgentTaskDispatchRequest,
    repo: &Option<String>,
    workspace: Option<&DispatchWorkspaceTarget>,
    client_context: &Value,
) -> Result<Value> {
    let mut config = Value::Object(defaults::load_config().settings.into_iter().collect());
    if let Some(spec) = &request.core.provider_config {
        let raw = read_text_spec(spec, "provider-config")?;
        let explicit = serde_json::from_str::<Value>(&raw).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task cook provider config".to_string()),
                Some(raw),
            )
        })?;

        if !explicit.is_object() {
            return Err(provider_config_must_be_object_error());
        }

        let map = config.as_object_mut().expect("global settings object");
        map.extend(
            explicit
                .as_object()
                .expect("explicit provider config object")
                .clone(),
        );
    }

    if !config.is_object() {
        return Err(provider_config_must_be_object_error());
    }

    let mut config = materialize_provider_config_refs(config)?;
    if !config.is_object() {
        return Err(Error::validation_invalid_argument(
            "provider-config",
            "agent-task cook --provider-config root must remain a JSON object after materializing configured refs",
            None,
            None,
        ));
    }
    let map = config.as_object_mut().expect("provider config object");
    map.entry("repo".to_string())
        .or_insert_with(|| serde_json::json!(repo));
    map.entry("workspace".to_string())
        .or_insert_with(|| serde_json::json!(workspace.map(|target| target.metadata.clone())));
    map.entry("workspace_required".to_string())
        .or_insert_with(|| serde_json::json!(workspace.is_some()));
    map.entry("workspace_root".to_string()).or_insert_with(|| {
        serde_json::json!(workspace.map(|target| target.root.display().to_string()))
    });
    map.entry("client_context".to_string())
        .or_insert_with(|| client_context.clone());
    if let Some(artifact_dependencies) = client_context.get("artifact_dependencies") {
        map.entry("artifact_dependencies".to_string())
            .or_insert_with(|| artifact_dependencies.clone());
    }
    map.entry("task_url".to_string())
        .or_insert_with(|| serde_json::json!(request.task_url));

    Ok(config)
}

pub(crate) fn dispatch_component_contracts(
    provider_config: &Value,
    client_context: &Value,
) -> Result<Vec<AgentTaskComponentContract>> {
    let mut contracts = Vec::new();
    collect_component_contracts_from_value(provider_config, "provider-config", &mut contracts)?;
    collect_component_contracts_from_value(client_context, "client-context", &mut contracts)?;
    if let Some(inputs) = client_context.get("inputs") {
        collect_component_contracts_from_value(inputs, "client-context.inputs", &mut contracts)?;
    }
    Ok(contracts)
}

pub(crate) fn dispatch_request_inputs(client_context: &Value) -> Value {
    client_context.get("inputs").cloned().unwrap_or(Value::Null)
}

fn collect_component_contracts_from_value(
    value: &Value,
    label: &str,
    contracts: &mut Vec<AgentTaskComponentContract>,
) -> Result<()> {
    for key in ["component_contracts", "runtime_component_contracts"] {
        let Some(raw) = value.get(key) else {
            continue;
        };
        let mut parsed: Vec<AgentTaskComponentContract> = serde_json::from_value(raw.clone())
            .map_err(|error| {
                Error::validation_invalid_argument(
                    format!("{label}.{key}"),
                    format!("agent-task cook {label}.{key} must be an array of component contracts: {error}"),
                    Some(raw.to_string()),
                    None,
                )
            })?;
        contracts.append(&mut parsed);
    }
    Ok(())
}

pub(crate) fn promote_component_contracts_to_provider_config(
    provider_config: &mut Value,
    component_contracts: &[AgentTaskComponentContract],
) {
    if component_contracts.is_empty() {
        return;
    }
    let Some(map) = provider_config.as_object_mut() else {
        return;
    };
    map.entry("component_contracts".to_string())
        .or_insert_with(|| serde_json::to_value(component_contracts).unwrap_or(Value::Null));
}

pub(crate) fn dispatch_secret_env(
    request: &AgentTaskDispatchRequest,
    provider_config: &Value,
) -> Vec<String> {
    let mut names = request.secret_env.clone();
    names.extend(provider_config_secret_env(provider_config));
    names.sort();
    names.dedup();
    names
}

fn provider_config_secret_env(provider_config: &Value) -> Vec<String> {
    let Some(config) = provider_config.as_object() else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(Value::Array(items)) => {
                names.extend(
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string)),
                );
            }
            Some(Value::String(name)) => names.push(name.clone()),
            _ => {}
        }
    }
    names.sort();
    names.dedup();
    names
}
