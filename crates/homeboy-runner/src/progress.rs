use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{json, Value};

use homeboy_core::redaction::RedactionPolicy;

pub(crate) const RUNNER_PROGRESS_LINE_PREFIX: &str = "HOMEBOY_RUNNER_PROGRESS ";
pub(crate) const RUNNER_PROGRESS_SCHEMA: &str = "homeboy/runner-progress/v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildProgressEnvelope {
    schema: String,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    current_item: Option<String>,
    #[serde(default)]
    completed: Option<u64>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    metadata: Option<Value>,
}

pub(crate) fn parse_child_progress_line(
    line: &str,
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> Option<Value> {
    let payload = line.strip_prefix(RUNNER_PROGRESS_LINE_PREFIX)?;
    let envelope: ChildProgressEnvelope = serde_json::from_str(payload).ok()?;
    if envelope.schema != RUNNER_PROGRESS_SCHEMA
        || envelope
            .phase
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 2048)
        || envelope
            .current_item
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 8192)
        || envelope
            .completed
            .zip(envelope.total)
            .is_some_and(|(completed, total)| completed > total)
        || (envelope.phase.is_none()
            && envelope.current_item.is_none()
            && envelope.completed.is_none()
            && envelope.total.is_none()
            && envelope.metadata.is_none())
    {
        return None;
    }

    let value = json!({
        "schema": RUNNER_PROGRESS_SCHEMA,
        "phase": envelope.phase,
        "current_item": envelope.current_item,
        "completed": envelope.completed,
        "total": envelope.total,
        "metadata": envelope.metadata,
    });
    Some(redact_progress_value(value, env, secret_env_names))
}

fn redact_progress_value(
    value: Value,
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> Value {
    let policy = RedactionPolicy::default();
    let mut secrets = env
        .iter()
        .filter_map(|(name, value)| {
            (!value.is_empty()
                && (policy.is_sensitive_key(name)
                    || secret_env_names.iter().any(|secret| secret == name)))
            .then_some(value.as_str())
        })
        .collect::<Vec<_>>();
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    redact_progress_strings(policy.redact_json(&value), &secrets, policy.replacement())
}

fn redact_progress_strings(value: Value, secrets: &[&str], replacement: &str) -> Value {
    match value {
        Value::String(mut value) => {
            for secret in secrets {
                value = value.replace(secret, replacement);
            }
            Value::String(value)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_progress_strings(value, secrets, replacement))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, redact_progress_strings(value, secrets, replacement)))
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_redacts_a_valid_child_progress_envelope() {
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), "secret-value".to_string());
        let line = format!(
            "{RUNNER_PROGRESS_LINE_PREFIX}{{\"schema\":\"{RUNNER_PROGRESS_SCHEMA}\",\"phase\":\"import\",\"current_item\":\"secret-value\",\"completed\":1,\"total\":3,\"metadata\":{{\"token\":\"abc\"}}}}"
        );

        let event = parse_child_progress_line(&line, &env, &["TOKEN".to_string()])
            .expect("valid progress event");

        assert_eq!(event["phase"], "import");
        assert_eq!(event["current_item"], "[REDACTED]");
        assert_eq!(event["metadata"]["token"], "[REDACTED]");
    }

    #[test]
    fn rejects_malformed_and_terminal_state_forging_lines() {
        let env = HashMap::new();
        assert!(parse_child_progress_line("ordinary output", &env, &[]).is_none());
        assert!(parse_child_progress_line(
            &format!("{RUNNER_PROGRESS_LINE_PREFIX}{{not-json}}"),
            &env,
            &[]
        )
        .is_none());
        assert!(parse_child_progress_line(
            &format!("{RUNNER_PROGRESS_LINE_PREFIX}{{\"schema\":\"{RUNNER_PROGRESS_SCHEMA}\",\"phase\":\"done\",\"status\":\"succeeded\"}}"),
            &env,
            &[]
        )
        .is_none());
    }
}
