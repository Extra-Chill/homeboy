use std::io::Read;

use clap::{Args, Subcommand};
use homeboy::core::agent_tasks::{
    dispatch_agent_tool_request, AgentToolPolicy, AgentToolRequest,
    HomeboyAgentToolControlPlaneDispatcher,
};
use serde_json::Value;

#[derive(Args, Debug)]
pub struct AgentTaskToolArgs {
    #[command(subcommand)]
    pub command: AgentTaskToolCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskToolCommand {
    /// Dispatch one agent tool request from stdin and emit a raw agent-tool-result JSON object.
    Dispatch(AgentTaskToolDispatchArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskToolDispatchArgs {}

pub fn dispatch_raw(_args: AgentTaskToolDispatchArgs) -> i32 {
    // homeboy-audit: allow-thin-command-adapter
    match dispatch_raw_result() {
        // homeboy-audit: allow-thin-command-adapter
        Ok(result) => {
            println!("{}", result);
            0
        }
        Err(message) => {
            eprintln!("{message}");
            2
        }
    }
}

fn dispatch_raw_result() -> Result<String, String> {
    // homeboy-audit: allow-thin-command-adapter
    let mut stdin = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin)
        .map_err(|error| format!("failed to read agent tool request from stdin: {error}"))?;

    let request: AgentToolRequest = serde_json::from_str(&stdin)
        .map_err(|error| format!("invalid agent tool request JSON: {error}"))?;
    let policy = policy_from_env()?;
    let outcome =
        dispatch_agent_tool_request(&policy, &request, &HomeboyAgentToolControlPlaneDispatcher); // homeboy-audit: allow-thin-command-adapter

    serde_json::to_string(&outcome.result)
        .map_err(|error| format!("failed to serialize agent tool result JSON: {error}"))
}

fn policy_from_env() -> Result<AgentToolPolicy, String> {
    match std::env::var("HOMEBOY_AGENT_TOOL_POLICY_JSON") {
        Ok(raw) if raw.trim().is_empty() => Ok(AgentToolPolicy::default()),
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|error| format!("invalid HOMEBOY_AGENT_TOOL_POLICY_JSON: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(AgentToolPolicy::default()),
        Err(std::env::VarError::NotUnicode(value)) => Err(format!(
            "HOMEBOY_AGENT_TOOL_POLICY_JSON is not valid unicode: {:?}",
            Value::String(value.to_string_lossy().to_string())
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::agent_tasks::{
        AgentToolExecutionLocation, AgentToolResultStatus, AGENT_TOOL_POLICY_SCHEMA,
        AGENT_TOOL_REQUEST_SCHEMA,
    };
    use serde_json::json;

    #[test]
    fn missing_policy_env_defaults_to_disabled() {
        std::env::remove_var("HOMEBOY_AGENT_TOOL_POLICY_JSON");

        assert_eq!(
            policy_from_env().expect("policy").default_location,
            AgentToolExecutionLocation::Disabled
        );
    }

    #[test]
    fn dispatch_result_is_denied_when_policy_disabled() {
        let policy: AgentToolPolicy = serde_json::from_value(json!({
            "schema": AGENT_TOOL_POLICY_SCHEMA,
            "default_location": "disabled"
        }))
        .expect("policy");
        let request: AgentToolRequest =
            serde_json::from_value(request_json("create_github_issue")).expect("request");

        let outcome =
            dispatch_agent_tool_request(&policy, &request, &HomeboyAgentToolControlPlaneDispatcher);

        assert_eq!(outcome.result.status, AgentToolResultStatus::Denied);
        assert_eq!(outcome.result.diagnostics[0].class, "agent_tool.disabled");
    }

    #[test]
    fn dispatch_result_validates_control_plane_tools() {
        let policy: AgentToolPolicy = serde_json::from_value(json!({
            "schema": AGENT_TOOL_POLICY_SCHEMA,
            "default_location": "control_plane"
        }))
        .expect("policy");
        let request: AgentToolRequest =
            serde_json::from_value(request_json("create_github_issue")).expect("request");

        let outcome =
            dispatch_agent_tool_request(&policy, &request, &HomeboyAgentToolControlPlaneDispatcher);

        assert_eq!(outcome.result.status, AgentToolResultStatus::Failed);
        assert_eq!(outcome.result.diagnostics[0].class, "agent_tool.validation");
    }

    fn request_json(tool: &str) -> Value {
        json!({
            "schema": AGENT_TOOL_REQUEST_SCHEMA,
            "request_id": "request-1",
            "task_id": "task-1",
            "tool": tool,
            "input": {}
        })
    }
}
