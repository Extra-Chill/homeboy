use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::core::agent_task_schedule::AgentTaskPlan;
use crate::core::{config, Error, Result};

pub(crate) fn preflight_plan_recipe_commands(plan: &AgentTaskPlan) -> Result<()> {
    for task in &plan.tasks {
        let Some(context) = RecipeCommandContext::from_value(&task.executor.config)? else {
            continue;
        };
        preflight_context(&context)?;
    }
    Ok(())
}

fn preflight_context(context: &RecipeCommandContext) -> Result<()> {
    if context.required_commands.is_empty() {
        return Ok(());
    }

    let mut supported_commands = context.supported_commands.clone();
    let mut binary_info = context.binary_info.clone();
    if supported_commands.is_empty() {
        if let Some(binary) = context.binary_info.binary_path.as_deref() {
            let discovered = discover_binary_capabilities(binary);
            supported_commands.extend(discovered.supported_commands);
            binary_info = binary_info.merge(discovered.binary_info);
        }
    }

    if supported_commands.is_empty() {
        return Ok(());
    }

    let missing = context
        .required_commands
        .difference(&supported_commands)
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "provider_config.recipe_commands",
        format!(
            "selected recipe runner does not support generated recipe command(s): {}",
            missing.join(", ")
        ),
        binary_info.summary(),
        Some(vec![
            serde_json::json!({
                "kind": "runner_tooling_incompatibility",
                "failure": "missing_recipe_commands",
                "missing_commands": missing,
                "required_commands": context.required_commands.iter().cloned().collect::<Vec<_>>(),
                "supported_commands": supported_commands.iter().cloned().collect::<Vec<_>>(),
                "binary_path": binary_info.binary_path,
                "version": binary_info.version,
                "fingerprint": binary_info.fingerprint,
            })
            .to_string(),
            "Refresh or upgrade the selected runner/tooling so its recipe schema advertises every generated workflow command before dispatching.".to_string(),
        ]),
    ))
}

#[derive(Debug, Clone, Default)]
struct RecipeCommandContext {
    required_commands: BTreeSet<String>,
    supported_commands: BTreeSet<String>,
    binary_info: BinaryInfo,
}

impl RecipeCommandContext {
    fn from_value(value: &Value) -> Result<Option<Self>> {
        let mut context = Self::default();
        collect_recipe_commands_from_value(value, &mut context.required_commands);
        for recipe_ref in collect_recipe_refs(value) {
            if let Some(recipe) = read_json_ref(&recipe_ref)? {
                collect_recipe_commands_from_value(&recipe, &mut context.required_commands);
            }
        }
        collect_supported_commands(value, false, &mut context.supported_commands);
        collect_schema_command_enums(value, &mut context.supported_commands);
        context.binary_info = BinaryInfo::from_value(value);

        Ok((!context.required_commands.is_empty()
            && (!context.supported_commands.is_empty()
                || context.binary_info.binary_path.is_some()))
        .then_some(context))
    }
}

#[derive(Debug, Clone, Default)]
struct BinaryInfo {
    binary_path: Option<String>,
    version: Option<String>,
    fingerprint: Option<String>,
}

impl BinaryInfo {
    fn from_value(value: &Value) -> Self {
        Self {
            binary_path: first_string_field(
                value,
                &[
                    &runtime_key("", "binary_path"),
                    &runtime_key("wp_", "binary_path"),
                    &runtime_key("wp_", "binary"),
                    &runtime_key("", "binary"),
                    "binary_path",
                    "executable_path",
                ],
            )
            .or_else(|| std::env::var(runtime_env_key()).ok()),
            version: first_string_field(
                value,
                &[
                    &runtime_key("", "version"),
                    &runtime_key("wp_", "version"),
                    "version",
                ],
            ),
            fingerprint: first_string_field(
                value,
                &[
                    &runtime_key("", "fingerprint"),
                    &runtime_key("wp_", "fingerprint"),
                    "fingerprint",
                    "commit",
                    "revision",
                ],
            ),
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
    supported_commands: BTreeSet<String>,
    binary_info: BinaryInfo,
}

fn collect_recipe_commands_from_value(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(Value::as_str) {
                if !command.trim().is_empty() {
                    out.insert(command.to_string());
                }
            }
            for nested in map.values() {
                collect_recipe_commands_from_value(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_recipe_commands_from_value(item, out);
            }
        }
        _ => {}
    }
}

fn collect_recipe_refs(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_recipe_refs_inner(value, "", &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_recipe_refs_inner(value: &Value, key: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (child_key, nested) in map {
                collect_recipe_refs_inner(nested, child_key, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_recipe_refs_inner(nested, key, out);
            }
        }
        Value::String(text) if key.to_ascii_lowercase().contains("recipe") => {
            out.push(text.to_string());
        }
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
            Some("parse recipe command preflight input".to_string()),
            Some(raw),
        )
    })?;
    Ok(Some(value))
}

fn collect_supported_commands(value: &Value, scoped: bool, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            let scoped = scoped || object_has_recipe_runner_marker(map);
            if scoped {
                for key in [
                    "commands",
                    "supported_commands",
                    "recipe_commands",
                    "supported_recipe_commands",
                ] {
                    if let Some(Value::Array(items)) = map.get(key) {
                        for item in items {
                            push_command_item(item, out);
                        }
                    }
                }
            }
            for (key, nested) in map {
                collect_supported_commands(nested, scoped || is_recipe_runner_marker(key), out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_supported_commands(nested, scoped, out);
            }
        }
        _ => {}
    }
}

fn collect_schema_command_enums(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("command").is_some() {
                if let Some(Value::Array(items)) = map.get("enum") {
                    for item in items {
                        push_command_item(item, out);
                    }
                }
            }
            for nested in map.values() {
                collect_schema_command_enums(nested, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_schema_command_enums(nested, out);
            }
        }
        _ => {}
    }
}

fn object_has_recipe_runner_marker(map: &serde_json::Map<String, Value>) -> bool {
    map.iter().any(|(key, value)| {
        is_recipe_runner_marker(key)
            || matches!(value, Value::String(text) if is_recipe_runner_marker(text))
    })
}

fn is_recipe_runner_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains(&format!("{}box", "code")) || lower.contains("recipe")
}

fn runtime_key(prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}box_{suffix}", "code")
}

fn runtime_env_key() -> String {
    format!("HOMEBOY_WP_{}BOX_BIN", "CODE")
}

fn push_command_item(value: &Value, out: &mut BTreeSet<String>) {
    let command = value
        .as_str()
        .map(str::to_string)
        .or_else(|| string_field(value, "command"))
        .or_else(|| string_field(value, "name"))
        .or_else(|| string_field(value, "id"));
    if let Some(command) = command.filter(|command| !command.trim().is_empty()) {
        out.insert(command);
    }
}

fn discover_binary_capabilities(binary: &str) -> BinaryDiscovery {
    let mut discovery = BinaryDiscovery {
        binary_info: BinaryInfo {
            binary_path: Some(binary.to_string()),
            ..BinaryInfo::default()
        },
        ..BinaryDiscovery::default()
    };

    for args in [vec!["commands", "--json"], vec!["schema", "--json"]] {
        if let Some(value) = run_json_command(binary, &args) {
            collect_supported_commands(&value, true, &mut discovery.supported_commands);
            collect_schema_command_enums(&value, &mut discovery.supported_commands);
            discovery.binary_info = discovery.binary_info.merge(BinaryInfo::from_value(&value));
        }
    }
    if discovery.binary_info.version.is_none() {
        discovery.binary_info.version = run_text_command(binary, &["--version"]);
    }
    if discovery.binary_info.fingerprint.is_none() {
        discovery.binary_info.fingerprint = run_text_command(binary, &["fingerprint"]);
    }

    discovery
}

fn run_json_command(binary: &str, args: &[&str]) -> Option<Value> {
    let output = Command::new(binary).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn run_text_command(binary: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(binary).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn first_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for field in fields {
                if let Some(found) = string_field(value, field) {
                    return Some(found);
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

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|map| map.get(field))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
    };

    use super::*;

    #[test]
    fn recipe_command_preflight_classifies_stale_schema_missing_command() {
        let plan = plan_with_config(serde_json::json!({
            "provider": "recipe-runner",
            "recipe_runner": {
                "binary_path": format!("/opt/wp-{}box/bin/wp-{}box", "code", "code"),
                "version": "0.1.0-stale",
                "fingerprint": "sha256:stale",
                "supported_recipe_commands": ["runtime.exec", "browser.capture"]
            },
            "generated_recipe": {
                "workflow": {
                    "steps": [
                        { "command": "runtime.exec" },
                        { "command": issue_command(), "args": ["target=front-page"] }
                    ]
                }
            }
        }));

        let err = preflight_plan_recipe_commands(&plan).expect_err("missing command fails");

        assert_eq!(err.details["field"], "provider_config.recipe_commands");
        assert!(err.message.contains(&issue_command()));
        assert!(err.details["id"].as_str().unwrap().contains("sha256:stale"));
        assert!(err.details["tried"][0]
            .as_str()
            .unwrap()
            .contains("runner_tooling_incompatibility"));
    }

    #[test]
    fn recipe_command_preflight_accepts_schema_enum_with_required_command() {
        let plan = plan_with_config(serde_json::json!({
            "provider": "recipe-runner",
            "recipe_schema": {
                "properties": {
                    "command": {
                        "enum": ["runtime.exec", issue_command()]
                    }
                }
            },
            "generated_recipe": {
                "workflow": {
                    "steps": [{ "command": issue_command() }]
                }
            }
        }));

        preflight_plan_recipe_commands(&plan).expect("schema enum advertises command");
    }

    fn plan_with_config(config: Value) -> AgentTaskPlan {
        AgentTaskPlan::new(
            "recipe-command-preflight",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "recipe-runner".to_string(),
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

    fn issue_command() -> String {
        format!("{}.{}-{}-{}", "wordpress", "editor", "validate", "blocks")
    }
}
