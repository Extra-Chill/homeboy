use std::collections::HashMap;

use crate::core::engine::validation;
use crate::core::error::{Error, Result};
use crate::core::project;
use crate::core::server::http::ApiClient;

use super::{
    build_action_env, execute_extension_command, load_extension, ExtensionExecutionMode,
    ExtensionScope,
};
use crate::core::extension::manifest::{ActionConfig, ActionType, HttpMethod};

pub(crate) fn execute_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let extension = load_extension(extension_id)?;

    if extension.actions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!("Extension '{}' has no actions defined", extension_id),
            Some(extension_id.to_string()),
            None,
        ));
    }

    let action = extension
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!(
                    "Action '{}' not found in extension '{}'",
                    action_id, extension_id
                ),
                Some(action_id.to_string()),
                None,
            )
        })?;

    let selected: Vec<serde_json::Value> = if let Some(data_str) = data {
        serde_json::from_str(data_str).map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse action data".to_string()))
        })?
    } else {
        Vec::new()
    };

    match action.action_type {
        ActionType::Api => {
            let pid = validation::require(
                project_id,
                "project",
                "--project is required for API actions",
            )?;

            let project = project::load(pid)?;
            let client = ApiClient::new(pid, &project.api)?;

            if action.requires_auth.unwrap_or(false) && !client.is_authenticated() {
                return Err(Error::validation_invalid_argument(
                    "auth",
                    "Not authenticated",
                    None,
                    Some(vec!["Run 'homeboy auth login --project <id>' first.".to_string()]),
                ));
            }

            let endpoint = validation::require(
                action.endpoint.as_ref(),
                "endpoint",
                "API action missing 'endpoint'",
            )?;

            let method = action.method.as_ref().unwrap_or(&HttpMethod::Post);
            let project = project::load(pid)?;
            let settings = ExtensionScope::effective_settings(extension_id, Some(&project), None)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;

            match method {
                HttpMethod::Get => client.get(endpoint),
                HttpMethod::Post => client.post(endpoint, &payload),
                HttpMethod::Put => client.put(endpoint, &payload),
                HttpMethod::Patch => client.patch(endpoint, &payload),
                HttpMethod::Delete => client.delete(endpoint),
            }
        }
        ActionType::Builtin => Err(Error::validation_invalid_argument(
            "action_id",
            format!("Action '{}' is a builtin action. Builtin actions run in the Desktop app, not the CLI.", action_id),
            Some(action_id.to_string()),
            None,
        )),
        ActionType::Command => {
            let command_template = validation::require(
                action.command.as_ref(),
                "command",
                "Command action missing 'command'",
            )?;
            let project = project_id.and_then(|pid| project::load(pid).ok());
            let component = None;
            let settings =
                ExtensionScope::effective_settings(extension_id, project.as_ref(), component)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;
            let extension_path = extension.extension_path.as_deref().unwrap_or(".");
            let vars = vec![("extension_path", extension_path)];

            let project_base_path = project_id
                .and_then(|pid| project::load(pid).ok())
                .and_then(|proj| proj.base_path.clone());

            let working_dir =
                crate::core::engine::text::json_path_str(&payload, &["release", "local_path"])
                    .unwrap_or(extension_path);

            let mut env = build_action_env(
                extension_id,
                project_id,
                &payload,
                Some(extension_path),
                project_base_path.as_deref(),
            );
            if let Some(response) = preflight_wordpress_release_publish_token(
                extension_id,
                action_id,
                &payload,
                &mut env,
            ) {
                return Ok(response);
            }

            let execution = execute_extension_command(
                command_template,
                &vars,
                Some(working_dir),
                &env,
                ExtensionExecutionMode::Captured,
            )?;
            Ok(serde_json::json!({
                "stdout": execution.output.stdout,
                "stderr": execution.output.stderr,
                "exitCode": execution.exit_code,
                "success": execution.success,
                "payload": payload
            }))
        }
    }
}

pub(crate) fn wordpress_release_publish_token_remediation() -> &'static str {
    "GH_TOKEN is required to push the WordPress release-latest mirror. Set GH_TOKEN with `export GH_TOKEN=\"$(gh auth token)\"`, or run `gh auth login` and rerun the release."
}

fn preflight_wordpress_release_publish_token(
    extension_id: &str,
    action_id: &str,
    payload: &serde_json::Value,
    env: &mut Vec<(String, String)>,
) -> Option<serde_json::Value> {
    if extension_id != "wordpress" || action_id != "release.publish" {
        return None;
    }

    let Some(token) = crate::core::git::github_token_from_env_or_gh() else {
        return Some(serde_json::json!({
            "success": false,
            "status": "missing_secret",
            "reason": wordpress_release_publish_token_remediation(),
            "payload": payload,
        }));
    };

    upsert_env(env, "GH_TOKEN", token);
    None
}

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some((_, existing)) = env.iter_mut().find(|(env_key, _)| env_key == key) {
        *existing = value;
        return;
    }

    env.push((key.to_string(), value));
}

fn interpolate_action_payload(
    action: &ActionConfig,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let payload_template = match &action.payload {
        Some(p) => p,
        None => {
            if let Some(payload) = payload {
                return Ok(payload.clone());
            }
            return Ok(serde_json::Value::Object(serde_json::Map::new()));
        }
    };

    let mut result = serde_json::Map::new();
    for (key, value) in payload_template {
        let interpolated = interpolate_payload_value(value, selected, settings, payload)?;
        result.insert(key.clone(), interpolated);
    }

    Ok(serde_json::Value::Object(result))
}

fn interpolate_payload_value(
    value: &serde_json::Value,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::String(template) => {
            if template == "{{selected}}" {
                Ok(serde_json::Value::Array(selected.to_vec()))
            } else if template.starts_with("{{settings.") && template.ends_with("}}") {
                let key = &template[11..template.len() - 2];
                Ok(settings
                    .get(key)
                    .cloned()
                    .unwrap_or(serde_json::Value::String(String::new())))
            } else if template.starts_with("{{payload.") && template.ends_with("}}") {
                let key = &template[10..template.len() - 2];
                Ok(payload
                    .and_then(|payload| payload.get(key))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else if template.starts_with("{{release.") && template.ends_with("}}") {
                let key = &template[10..template.len() - 2];
                Ok(payload
                    .and_then(|p| p.get("release"))
                    .and_then(|r| r.get(key))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else {
                Ok(serde_json::Value::String(template.clone()))
            }
        }
        serde_json::Value::Array(arr) => {
            let interpolated: Result<Vec<serde_json::Value>> = arr
                .iter()
                .map(|v| interpolate_payload_value(v, selected, settings, payload))
                .collect();
            Ok(serde_json::Value::Array(interpolated?))
        }
        serde_json::Value::Object(obj) => {
            let mut result = serde_json::Map::new();
            for (k, v) in obj {
                result.insert(
                    k.clone(),
                    interpolate_payload_value(v, selected, settings, payload)?,
                );
            }
            Ok(serde_json::Value::Object(result))
        }
        _ => Ok(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::upsert_env;

    #[test]
    fn upsert_env_adds_missing_key() {
        let mut env = vec![("A".to_string(), "one".to_string())];

        upsert_env(&mut env, "GH_TOKEN", "token".to_string());

        assert_eq!(
            env,
            vec![
                ("A".to_string(), "one".to_string()),
                ("GH_TOKEN".to_string(), "token".to_string()),
            ]
        );
    }

    #[test]
    fn upsert_env_replaces_existing_key() {
        let mut env = vec![("GH_TOKEN".to_string(), "old".to_string())];

        upsert_env(&mut env, "GH_TOKEN", "new".to_string());

        assert_eq!(env, vec![("GH_TOKEN".to_string(), "new".to_string())]);
    }
}
