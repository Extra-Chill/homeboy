use super::capabilities::{RunnerCapabilityPreflight, RunnerRequiredTool};
use super::Runner;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerToolSpec {
    pub tool: Option<RunnerRequiredTool>,
    pub id: String,
    pub capability_id: String,
    pub check_id: String,
    pub command: String,
    pub version_args: Vec<String>,
    pub required: bool,
    pub remediation: String,
    pub capability_remediation: String,
}

pub struct RunnerToolRegistry;

impl RunnerToolRegistry {
    pub fn required_tools(
        runner: &Runner,
        preflight: &RunnerCapabilityPreflight,
    ) -> Vec<RunnerRequiredTool> {
        let mut tools = Vec::new();
        push_unique(&mut tools, RunnerRequiredTool::homeboy());
        push_unique(&mut tools, RunnerRequiredTool::git());
        for tool in declared_tool_specs(runner)
            .into_iter()
            .filter_map(|spec| spec.tool)
        {
            push_unique(&mut tools, tool);
        }
        for tool in &preflight.required_tools {
            push_unique(&mut tools, tool.clone());
        }
        tools
    }

    pub fn doctor_tools(runner: &Runner) -> Vec<RunnerToolSpec> {
        let mut specs = intrinsic_tool_specs();
        specs.extend(declared_tool_specs(runner));
        specs
    }

    pub fn spec_for_required_tool(tool: &RunnerRequiredTool) -> Option<RunnerToolSpec> {
        intrinsic_tool_specs()
            .into_iter()
            .find(|spec| spec.tool.as_ref() == Some(tool))
    }
}

fn intrinsic_tool_specs() -> Vec<RunnerToolSpec> {
    vec![
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::homeboy()),
            id: "homeboy".to_string(),
            capability_id: "homeboy".to_string(),
            check_id: "homeboy".to_string(),
            command: "homeboy".to_string(),
            version_args: vec!["--version".to_string()],
            required: true,
            remediation: "Install Homeboy on the remote runner or configure runner.homeboy_path/server.env.PATH".to_string(),
            capability_remediation: "Install Homeboy on the runner and ensure the configured homeboy_path works.".to_string(),
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::git()),
            id: "git".to_string(),
            capability_id: "git".to_string(),
            check_id: "tool.git".to_string(),
            command: "git".to_string(),
            version_args: vec!["--version".to_string()],
            required: true,
            remediation: "Install git and ensure it is on PATH".to_string(),
            capability_remediation: "Install git and ensure it is on the runner PATH.".to_string(),
        },
    ]
}

fn declared_tool_specs(runner: &Runner) -> Vec<RunnerToolSpec> {
    let Some(value) = runner.resources.get("tools") else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items.iter().filter_map(declared_tool_spec).collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(id, value)| declared_tool_spec_with_id(id, value))
            .collect(),
        _ => Vec::new(),
    }
}

fn declared_tool_spec(value: &Value) -> Option<RunnerToolSpec> {
    let id = value.get("id").and_then(Value::as_str)?.trim();
    declared_tool_spec_with_id(id, value)
}

fn declared_tool_spec_with_id(id: &str, value: &Value) -> Option<RunnerToolSpec> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .trim()
        .to_string();
    if command.is_empty() {
        return None;
    }
    let version_args = value
        .get("version_args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec!["--version".to_string()]);
    let tool = RunnerRequiredTool::new(id);
    Some(RunnerToolSpec {
        tool: Some(tool),
        id: id.to_string(),
        capability_id: value
            .get("capability_id")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        check_id: value
            .get("check_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("tool.{id}")),
        command,
        version_args,
        required: value
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        remediation: value
            .get("remediation")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Install '{id}' and ensure it is on PATH")),
        capability_remediation: value
            .get("capability_remediation")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Install '{id}' on the runner and ensure it is on PATH.")),
    })
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}
