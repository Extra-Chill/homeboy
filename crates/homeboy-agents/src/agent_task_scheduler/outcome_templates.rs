//! Outcome normalization, typed-artifact extraction, template rendering, and
//! status-detection helpers for the agent-task scheduler.
//!
//! These functions inspect provider outcomes/results to decide materializability,
//! detect nested failed-executor status, recognize empty/incomplete runs, match
//! required typed artifacts, and render output-binding templates. They are split
//! from the scheduling engine so the value-shaping concerns stay together.
//! Helpers are `pub(super)` so the scheduler loop, scheduling engine, and tests
//! can reach them without widening the crate-public surface.

use serde_json::{Map, Value};
use std::collections::HashMap;

use super::*;
use homeboy_core::config::value_type_name;

pub(super) fn render_template_value(raw: &str, bindings: &HashMap<String, Value>) -> Option<Value> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?.trim();
    let name = inner.strip_prefix("outputs.")?.trim();
    bindings.get(name).cloned()
}

pub(super) fn render_template_string(raw: &str, bindings: &HashMap<String, Value>) -> String {
    let mut rendered = raw.to_string();
    for (name, value) in bindings {
        let replacement = template_replacement(value);
        let compact = format!("{{{{outputs.{name}}}}}");
        let spaced = format!("{{{{ outputs.{name} }}}}");
        rendered = rendered.replace(&compact, &replacement);
        rendered = rendered.replace(&spaced, &replacement);
    }
    rendered
}

pub(super) fn template_replacement(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    if let Some(value) = value.as_str() {
        return value.to_string();
    }
    if matches!(
        value_type_name(value),
        "number" | "bool" | "array" | "object"
    ) {
        return value.to_string();
    }
    String::new()
}

pub(super) fn mark_generated_from_outputs(
    request: &mut AgentTaskRequest,
    dependencies: &AgentTaskOutputDependencies,
    bindings: &HashMap<String, Value>,
) {
    if !request.metadata.is_object() {
        request.metadata = Value::Object(Map::new());
    }
    let metadata = request.metadata.as_object_mut().expect("metadata object");
    metadata.insert("generated_from_outputs".to_string(), Value::Bool(true));
    metadata.insert(
        "depends_on".to_string(),
        Value::Array(
            dependencies
                .depends_on
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    metadata.insert(
        "resolved_output_bindings".to_string(),
        Value::Object(
            bindings
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        ),
    );
}
