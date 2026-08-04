use super::runner_readiness::resolve_executable_candidate;
use super::*;
use crate::agent_task::{
    AgentTaskRuntimeTool, AgentToolExecutionLocation, ResolvedAgentTaskRuntimeTool,
    AGENT_TASK_RUNTIME_TOOL_SCHEMA, RESOLVED_AGENT_TASK_RUNTIME_TOOL_SCHEMA,
};
use crate::agent_task_process_containment::AgentTaskProcessContainment;
use std::process::{Command, Stdio};

pub(crate) struct RuntimeToolResolutionError {
    pub(super) class: &'static str,
    pub(super) message: String,
    pub(super) data: Value,
}

pub(crate) fn resolve_runtime_tools(
    request: &mut AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<(), RuntimeToolResolutionError> {
    let mut resolved = Vec::new();
    for tool in &request.request.runtime_tools {
        validate_tool(tool)?;
        if request
            .request
            .policy
            .tools
            .execution_location_for(&tool.id)
            != AgentToolExecutionLocation::Runner
        {
            return Err(RuntimeToolResolutionError {
                class: "agent_task.runtime_tool_not_authorized",
                message: format!(
                    "runtime tool '{}' is attached but is not authorized for runner execution by agent tool policy",
                    tool.id
                ),
                data: json!({ "tool": tool.id, "execution_location": request.request.policy.tools.execution_location_for(&tool.id) }),
            });
        }
        let missing: Vec<String> = tool
            .required_capabilities
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(RuntimeToolResolutionError {
                class: "agent_task.capability_missing",
                message: format!(
                    "provider '{}' cannot attach runtime tool '{}'; missing capabilities: {}",
                    provider.id,
                    tool.id,
                    missing.join(", ")
                ),
                data: json!({ "tool": tool.id, "provider": provider.id, "missing_capabilities": missing }),
            });
        }
        let executable = resolve_executable_candidate(&tool.command[0]).ok_or_else(|| {
            RuntimeToolResolutionError {
                class: "agent_task.runtime_tool_executable_missing",
                message: format!(
                    "runtime tool '{}' executable '{}' is unavailable on this execution host",
                    tool.id, tool.command[0]
                ),
                data: json!({ "tool": tool.id, "command": tool.command, "readiness": "executable_missing" }),
            }
        })?;
        let version = probe_version(tool, &executable)?;
        for secret in &tool.secret_env {
            if !request.request.executor.secret_env.contains(secret) {
                request.request.executor.secret_env.push(secret.clone());
            }
        }
        resolved.push(ResolvedAgentTaskRuntimeTool {
            schema: RESOLVED_AGENT_TASK_RUNTIME_TOOL_SCHEMA.to_string(),
            id: tool.id.clone(),
            transport: tool.transport.clone(),
            executable,
            version,
            capabilities: tool.required_capabilities.clone(),
            env_names: tool.env.keys().cloned().collect(),
            secret_env_names: tool.secret_env.clone(),
            readiness: "ready".to_string(),
            lifecycle: tool.lifecycle,
        });
    }
    if !resolved.is_empty() {
        if request.request.metadata.is_null() {
            request.request.metadata = json!({});
        }
        let metadata =
            request
                .request
                .metadata
                .as_object_mut()
                .ok_or_else(|| RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_metadata_invalid",
                    message: "runtime tool resolution requires task metadata to be an object"
                        .to_string(),
                    data: Value::Null,
                })?;
        metadata.insert(
            "resolved_runtime_tools".to_string(),
            serde_json::to_value(resolved).expect("resolved runtime tools serialize"),
        );
    }
    Ok(())
}

fn validate_tool(tool: &AgentTaskRuntimeTool) -> Result<(), RuntimeToolResolutionError> {
    if tool.schema != AGENT_TASK_RUNTIME_TOOL_SCHEMA
        || !valid_identifier(&tool.id)
        || tool.transport != "stdio"
        || tool.command.iter().any(|part| part.trim().is_empty())
        || tool.command.is_empty()
        || tool.timeout_ms == Some(0)
        || tool.secret_env.iter().any(|name| !valid_env_name(name))
        || tool
            .env
            .keys()
            .any(|name| !valid_env_name(name) || sensitive_name(name))
    {
        return Err(RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_invalid",
            message: format!("runtime tool '{}' has an invalid declaration", tool.id),
            data: json!({ "tool": tool.id, "transport": tool.transport }),
        });
    }
    Ok(())
}

fn probe_version(
    tool: &AgentTaskRuntimeTool,
    executable: &str,
) -> Result<Option<String>, RuntimeToolResolutionError> {
    if tool.readiness.version_command.is_empty() {
        return Ok(None);
    }
    let mut command = Command::new(executable);
    command
        .args(&tool.readiness.version_command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut containment = AgentTaskProcessContainment::prepare(&mut command).map_err(|error| {
        RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_readiness_failed",
            message: format!(
                "could not contain readiness probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
        }
    })?;
    let mut child = command
        .spawn()
        .map_err(|error| RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_readiness_failed",
            message: format!(
                "could not start readiness probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
        })?;
    if let Err(error) = containment.attach(&child) {
        let _ = containment.terminate_live(&mut child);
        return Err(RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_readiness_failed",
            message: format!(
                "could not guard readiness probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
        });
    }
    let timeout = std::time::Duration::from_millis(tool.timeout_ms.unwrap_or(20_000));
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = containment.reap_after_exit();
                let output = child.stdout.take().and_then(|mut stdout| {
                    let mut value = String::new();
                    std::io::Read::read_to_string(&mut stdout, &mut value).ok()?;
                    Some(value.trim().to_string())
                });
                return Ok(output.filter(|value| !value.is_empty()));
            }
            Ok(Some(_)) | Err(_) => {
                let _ = containment.reap_after_exit();
                return Err(RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_readiness_failed",
                    message: format!("readiness probe failed for runtime tool '{}'", tool.id),
                    data: json!({ "tool": tool.id, "readiness": "version_command_failed" }),
                });
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = containment.terminate_live(&mut child);
                return Err(RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_readiness_timeout",
                    message: format!("readiness probe timed out for runtime tool '{}'", tool.id),
                    data: json!({ "tool": tool.id, "timeout_ms": timeout.as_millis() }),
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        })
}

fn sensitive_name(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "credential",
        "api_key",
        "authorization",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}
