use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{config, Error, Result};

use super::resolution::select_provider;
use super::{
    AgentTaskExecutorProvider, AgentTaskProviderConfigBinaryProbe,
    AgentTaskProviderConfigPreflight, AgentTaskProviderConfigValueCollector,
};

pub(crate) fn preflight_plan_provider_config_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Result<()> {
    for task in &plan.tasks {
        let Some(provider) = select_provider(providers, task) else {
            continue;
        };
        for rule in &provider.config_preflights {
            preflight_rule(rule, &task.executor.config)?;
        }
    }
    Ok(())
}

fn preflight_rule(rule: &AgentTaskProviderConfigPreflight, value: &Value) -> Result<()> {
    let mut required = collect_values(value, &rule.required_values, false);
    for reference in collect_references(value, &rule.reference_key_contains) {
        if let Some(referenced) = read_json_ref(&reference)? {
            required.extend(collect_values(&referenced, &rule.required_values, false));
        }
    }
    if required.is_empty() {
        return Ok(());
    }

    let mut supported = collect_values(value, &rule.supported_values, false);
    let mut binary_info = BinaryInfo::default();
    if let Some(probe) = &rule.binary_probe {
        binary_info = BinaryInfo::from_value(value, probe);
        if supported.is_empty() {
            if let Some(binary) = binary_info.binary_path.as_deref() {
                let discovered =
                    discover_binary_capabilities(binary, probe, &rule.supported_values);
                supported.extend(discovered.supported_values);
                binary_info = binary_info.merge(discovered.binary_info);
            }
        }
    }

    if supported.is_empty() {
        return Ok(());
    }

    let missing = required.difference(&supported).cloned().collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    let mut hints = vec![serde_json::json!({
        "kind": "provider_config_preflight_failed",
        "preflight_id": rule.id,
        "missing_values": missing,
        "required_values": required.iter().cloned().collect::<Vec<_>>(),
        "supported_values": supported.iter().cloned().collect::<Vec<_>>(),
        "binary_path": binary_info.binary_path,
        "version": binary_info.version,
        "fingerprint": binary_info.fingerprint,
    })
    .to_string()];
    if let Some(remediation) = rule.remediation.as_ref().filter(|value| !value.is_empty()) {
        hints.push(remediation.clone());
    }

    Err(Error::validation_invalid_argument(
        format!("provider_config.preflight.{}", rule.id),
        format!(
            "provider config preflight '{}' failed: unsupported generated value(s): {}",
            rule.label.as_deref().unwrap_or(&rule.id),
            missing.join(", ")
        ),
        binary_info.summary(),
        Some(hints),
    ))
}

#[derive(Debug, Clone, Default)]
struct BinaryInfo {
    binary_path: Option<String>,
    version: Option<String>,
    fingerprint: Option<String>,
}

impl BinaryInfo {
    fn from_value(value: &Value, probe: &AgentTaskProviderConfigBinaryProbe) -> Self {
        Self {
            binary_path: first_string_field(value, &probe.path_keys).or_else(|| {
                probe
                    .path_env
                    .iter()
                    .find_map(|key| std::env::var(key).ok())
            }),
            version: first_string_field(value, &probe.version_keys),
            fingerprint: first_string_field(value, &probe.fingerprint_keys),
        }
    }

    fn merge(self, discovered: BinaryInfo) -> Self {
        Self {
            binary_path: self.binary_path.or(discovered.binary_path),
            version: self.version.or(discovered.version),
            fingerprint: self.fingerprint.or(discovered.fingerprint),
        }
    }

    fn summary(&self) -> Option<String> {
        let parts = [
            self.binary_path
                .as_ref()
                .map(|value| format!("binary={value}")),
            self.version
                .as_ref()
                .map(|value| format!("version={value}")),
            self.fingerprint
                .as_ref()
                .map(|value| format!("fingerprint={value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

#[derive(Debug, Clone, Default)]
struct BinaryDiscovery {
    supported_values: BTreeSet<String>,
    binary_info: BinaryInfo,
}

fn collect_values(
    value: &Value,
    collector: &AgentTaskProviderConfigValueCollector,
    scoped: bool,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_values_inner(value, collector, scoped, &mut out);
    out
}

fn collect_values_inner(
    value: &Value,
    collector: &AgentTaskProviderConfigValueCollector,
    scoped: bool,
    out: &mut BTreeSet<String>,
) {
    match value {
        Value::Object(map) => {
            let scoped = scoped || object_matches_scope(map, collector);
            for key in &collector.keys {
                if let Some(item) = map.get(key) {
                    push_value_item(item, out);
                }
            }
            if scoped {
                for key in &collector.scoped_keys {
                    if let Some(item) = map.get(key) {
                        push_value_item(item, out);
                    }
                }
            }
            for enum_key in &collector.enum_keys {
                if map.get(enum_key).is_some() {
                    if let Some(Value::Array(items)) = map.get("enum") {
                        for item in items {
                            push_value_item(item, out);
                        }
                    }
                }
            }
            for (key, nested) in map {
                let child_scoped = scoped || matches_marker(key, &collector.scope_key_contains);
                collect_values_inner(nested, collector, child_scoped, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_values_inner(item, collector, scoped, out);
            }
        }
        _ => {}
    }
}

fn object_matches_scope(
    map: &serde_json::Map<String, Value>,
    collector: &AgentTaskProviderConfigValueCollector,
) -> bool {
    map.iter().any(|(key, value)| {
        matches_marker(key, &collector.scope_key_contains)
            || value
                .as_str()
                .is_some_and(|text| matches_marker(text, &collector.scope_value_contains))
    })
}

fn matches_marker(value: &str, markers: &[String]) -> bool {
    let lower = value.to_ascii_lowercase();
    markers
        .iter()
        .any(|marker| lower.contains(&marker.to_ascii_lowercase()))
}

fn collect_references(value: &Value, key_markers: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    collect_references_inner(value, "", key_markers, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_references_inner(
    value: &Value,
    key: &str,
    key_markers: &[String],
    out: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            for (child_key, nested) in map {
                collect_references_inner(nested, child_key, key_markers, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_references_inner(item, key, key_markers, out);
            }
        }
        Value::String(text) if matches_marker(key, key_markers) => out.push(text.to_string()),
        _ => {}
    }
}

fn read_json_ref(spec: &str) -> Result<Option<Value>> {
    if spec.trim().is_empty() || !(spec.ends_with(".json") || spec.starts_with('@')) {
        return Ok(None);
    }
    let path_spec = spec.to_string();
    let path = path_spec.strip_prefix('@').unwrap_or(&path_spec);
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let raw = config::read_json_spec_to_string(&path_spec)?;
    let value = serde_json::from_str(&raw).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some("parse provider config preflight input".to_string()),
            Some(raw),
        )
    })?;
    Ok(Some(value))
}

fn push_value_item(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) if !text.trim().is_empty() => {
            out.insert(text.to_string());
        }
        Value::Array(items) => {
            for item in items {
                push_value_item(item, out);
            }
        }
        Value::Object(map) => {
            for key in ["value", "name", "id"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        out.insert(text.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

fn discover_binary_capabilities(
    binary: &str,
    probe: &AgentTaskProviderConfigBinaryProbe,
    supported_values: &AgentTaskProviderConfigValueCollector,
) -> BinaryDiscovery {
    let mut discovery = BinaryDiscovery {
        binary_info: BinaryInfo {
            binary_path: Some(binary.to_string()),
            ..BinaryInfo::default()
        },
        ..BinaryDiscovery::default()
    };

    for args in &probe.json_commands {
        if let Some(value) = run_json_command(binary, args) {
            discovery
                .supported_values
                .extend(collect_values(&value, supported_values, true));
            discovery.binary_info = discovery
                .binary_info
                .merge(BinaryInfo::from_value(&value, probe));
        }
    }
    if discovery.binary_info.version.is_none() && !probe.version_command.is_empty() {
        discovery.binary_info.version = run_text_command(binary, &probe.version_command);
    }
    if discovery.binary_info.fingerprint.is_none() && !probe.fingerprint_command.is_empty() {
        discovery.binary_info.fingerprint = run_text_command(binary, &probe.fingerprint_command);
    }

    discovery
}

fn run_json_command(binary: &str, args: &[String]) -> Option<Value> {
    let output = Command::new(binary).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn run_text_command(binary: &str, args: &[String]) -> Option<String> {
    let output = Command::new(binary).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn first_string_field(value: &Value, fields: &[String]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for field in fields {
                if let Some(found) = map
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    return Some(found.to_string());
                }
            }
            map.values()
                .find_map(|nested| first_string_field(nested, fields))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| first_string_field(nested, fields)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
    };

    use super::*;

    #[test]
    fn provider_config_preflight_classifies_stale_schema_missing_value() {
        let plan = plan_with_config(serde_json::json!({
            "provider": "sample-runner",
            "sample_runner": {
                "binary_path": "/opt/sample/bin/sample",
                "version": "0.1.0-stale",
                "fingerprint": "sha256:stale",
                "supported_operations": ["runtime.exec", "browser.capture"]
            },
            "generated_plan": {
                "workflow": {
                    "steps": [
                        { "operation": "runtime.exec" },
                        { "operation": "editor.validate-blocks" }
                    ]
                }
            }
        }));
        let providers = vec![provider(rule())];

        let err = preflight_plan_provider_config_with_providers(&plan, &providers)
            .expect_err("missing value fails");

        assert_eq!(
            err.details["field"],
            "provider_config.preflight.sample-operations"
        );
        assert!(err.message.contains("editor.validate-blocks"));
        assert!(err.details["id"].as_str().unwrap().contains("sha256:stale"));
        assert!(err.details["tried"][0]
            .as_str()
            .unwrap()
            .contains("provider_config_preflight_failed"));
    }

    #[test]
    fn provider_config_preflight_accepts_schema_enum_with_required_value() {
        let plan = plan_with_config(serde_json::json!({
            "provider": "sample-runner",
            "sample_schema": {
                "properties": {
                    "operation": {
                        "enum": ["runtime.exec", "editor.validate-blocks"]
                    }
                }
            },
            "generated_plan": {
                "workflow": {
                    "steps": [{ "operation": "editor.validate-blocks" }]
                }
            }
        }));
        let providers = vec![provider(rule())];

        preflight_plan_provider_config_with_providers(&plan, &providers)
            .expect("schema enum advertises value");
    }

    fn provider(rule: AgentTaskProviderConfigPreflight) -> AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({
            "id": "sample.default",
            "backend": "sample-runner",
            "config_preflights": [rule]
        }))
        .expect("provider")
    }

    fn rule() -> AgentTaskProviderConfigPreflight {
        serde_json::from_value(serde_json::json!({
            "id": "sample-operations",
            "label": "Sample operations",
            "required_values": {
                "keys": ["operation"]
            },
            "supported_values": {
                "scoped_keys": ["supported_operations"],
                "scope_key_contains": ["runner"],
                "scope_value_contains": ["runner"],
                "enum_keys": ["operation"]
            },
            "reference_key_contains": ["plan"],
            "binary_probe": {
                "path_keys": ["binary_path", "binary"],
                "json_commands": [["operations", "--json"], ["schema", "--json"]],
                "version_command": ["--version"],
                "fingerprint_command": ["fingerprint"],
                "version_keys": ["version"],
                "fingerprint_keys": ["fingerprint"]
            }
        }))
        .expect("rule")
    }

    fn plan_with_config(config: Value) -> AgentTaskPlan {
        AgentTaskPlan::new(
            "provider-config-preflight",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "sample-runner".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace {
                    mode: AgentTaskWorkspaceMode::Ephemeral,
                    root: None,
                    slug: None,
                    kind: None,
                    component_id: None,
                    branch: None,
                    base_ref: None,
                    task_url: None,
                    cleanup: None,
                    attempt: None,
                    materialization: Value::Null,
                },
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }
}
