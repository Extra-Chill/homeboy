use super::runner_readiness::resolve_executable_candidate;
use super::*;
use crate::agent_task::{
    AgentCommandDecision, AgentTaskRuntimeTool, AgentTaskRuntimeToolProbeEvidence,
    AgentToolExecutionLocation, ResolvedAgentTaskRuntimeTool, AGENT_TASK_RUNTIME_TOOL_SCHEMA,
    RESOLVED_AGENT_TASK_RUNTIME_TOOL_SCHEMA,
};
use crate::agent_task_process_containment::AgentTaskProcessContainment;
use std::process::{Command, Stdio};

pub(crate) struct RuntimeToolResolutionError {
    pub(super) class: &'static str,
    pub(super) message: String,
    pub(super) data: Value,
    pub(super) failure_classification: AgentTaskFailureClassification,
}

pub(crate) fn resolve_runtime_tools(
    request: &mut AgentTaskExecutorRequest,
    _provider: &AgentTaskExecutorProvider,
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
                failure_classification: AgentTaskFailureClassification::CapabilityMissing,
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
                failure_classification: AgentTaskFailureClassification::CapabilityMissing,
            }
        })?;
        let readiness_command = std::iter::once(executable.as_str())
            .chain(readiness_command_prefix(tool).iter().map(String::as_str))
            .chain(tool.readiness.version_command.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        if let AgentCommandDecision::Denied(denial) = request
            .request
            .policy
            .tools
            .evaluate_command(&readiness_command)
        {
            return Err(RuntimeToolResolutionError {
                class: "agent_task.runtime_tool_command_denied",
                message: denial.message(),
                data: json!({ "tool": tool.id, "command": denial.command, "reason": denial.reason }),
                failure_classification: AgentTaskFailureClassification::InvalidInput,
            });
        }
        let version = probe_version(tool, &executable)?;
        let capability_probe =
            probe_capabilities(tool, &executable, &request.request.policy.tools)?;
        for secret in &tool.secret_env {
            if !request.request.executor.secret_env.contains(secret) {
                request.request.executor.secret_env.push(secret.clone());
            }
        }
        resolved.push(ResolvedAgentTaskRuntimeTool {
            schema: RESOLVED_AGENT_TASK_RUNTIME_TOOL_SCHEMA.to_string(),
            id: tool.id.clone(),
            transport: tool.transport.clone(),
            executable: executable.clone(),
            argv: std::iter::once(executable.clone())
                .chain(tool.command.iter().skip(1).cloned())
                .collect(),
            env: tool.env.clone(),
            version,
            capabilities: capability_probe
                .as_ref()
                .map(|_| tool.required_capabilities.clone())
                .unwrap_or_default(),
            capability_probe,
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
                    failure_classification: AgentTaskFailureClassification::InvalidInput,
                })?;
        metadata.insert(
            "runtime_tool_attachment".to_string(),
            json!({ "count": resolved.len() }),
        );
        let readiness = resolved
            .iter()
            .filter(|tool| !tool.capabilities.is_empty() && tool.capability_probe.is_some())
            .map(|tool| (tool.id.clone(), json!({ "state": "ready" })))
            .collect::<serde_json::Map<String, Value>>();
        if !readiness.is_empty() {
            metadata.insert(
                "attached_tool_readiness".to_string(),
                Value::Object(readiness),
            );
        }
        request.resolved_runtime_tools = resolved;
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
        || (!tool.required_capabilities.is_empty() && tool.readiness.capability_probe.is_none())
    {
        return Err(RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_invalid",
            message: format!("runtime tool '{}' has an invalid declaration", tool.id),
            data: json!({ "tool": tool.id, "transport": tool.transport }),
            failure_classification: AgentTaskFailureClassification::InvalidInput,
        });
    }
    Ok(())
}

fn probe_capabilities(
    tool: &AgentTaskRuntimeTool,
    executable: &str,
    policy: &crate::agent_task::AgentToolPolicy,
) -> Result<Option<AgentTaskRuntimeToolProbeEvidence>, RuntimeToolResolutionError> {
    let Some(probe) = &tool.readiness.capability_probe else {
        return Ok(None);
    };
    let command = std::iter::once(executable)
        .chain(readiness_command_prefix(tool).iter().map(String::as_str))
        .chain(probe.argv.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    if let AgentCommandDecision::Denied(denial) = policy.evaluate_command(&command) {
        return Err(RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_command_denied",
            message: denial.message(),
            data: json!({ "tool": tool.id, "command": denial.command, "reason": denial.reason }),
            failure_classification: AgentTaskFailureClassification::InvalidInput,
        });
    }
    let mut command = Command::new(executable);
    command
        .args(readiness_command_prefix(tool))
        .args(&probe.argv)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_probe_environment(&mut command, tool);
    let mut containment = AgentTaskProcessContainment::prepare(&mut command).map_err(|error| {
        RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_capability_probe_failed",
            message: format!(
                "could not contain capability probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
            failure_classification: AgentTaskFailureClassification::Provider,
        }
    })?;
    let mut child = command
        .spawn()
        .map_err(|error| RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_capability_probe_failed",
            message: format!(
                "could not start capability probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
            failure_classification: AgentTaskFailureClassification::Provider,
        })?;
    if let Err(error) = containment.attach(&child) {
        let _ = containment.terminate_live(&mut child);
        return Err(RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_capability_probe_failed",
            message: format!(
                "could not guard capability probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
            failure_classification: AgentTaskFailureClassification::Provider,
        });
    }
    let timeout = std::time::Duration::from_millis(tool.timeout_ms.unwrap_or(20_000));
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = containment.reap_after_exit();
                return Ok(Some(AgentTaskRuntimeToolProbeEvidence {
                    status: "succeeded".to_string(),
                    argv: probe.argv.clone(),
                }));
            }
            Ok(Some(_)) | Err(_) => {
                let _ = containment.reap_after_exit();
                return Err(RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_capability_probe_failed",
                    message: format!("capability probe failed for runtime tool '{}'", tool.id),
                    data: json!({ "tool": tool.id }),
                    failure_classification: AgentTaskFailureClassification::CapabilityMissing,
                });
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = containment.terminate_live(&mut child);
                return Err(RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_capability_probe_timeout",
                    message: format!("capability probe timed out for runtime tool '{}'", tool.id),
                    data: json!({ "tool": tool.id, "timeout_ms": timeout.as_millis() }),
                    failure_classification: AgentTaskFailureClassification::Timeout,
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
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
        .args(readiness_command_prefix(tool))
        .args(&tool.readiness.version_command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    apply_probe_environment(&mut command, tool);
    let mut containment = AgentTaskProcessContainment::prepare(&mut command).map_err(|error| {
        RuntimeToolResolutionError {
            class: "agent_task.runtime_tool_readiness_failed",
            message: format!(
                "could not contain readiness probe for runtime tool '{}': {error}",
                tool.id
            ),
            data: json!({ "tool": tool.id }),
            failure_classification: AgentTaskFailureClassification::Provider,
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
            failure_classification: AgentTaskFailureClassification::Provider,
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
            failure_classification: AgentTaskFailureClassification::Provider,
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
                    failure_classification: AgentTaskFailureClassification::CapabilityMissing,
                });
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = containment.terminate_live(&mut child);
                return Err(RuntimeToolResolutionError {
                    class: "agent_task.runtime_tool_readiness_timeout",
                    message: format!("readiness probe timed out for runtime tool '{}'", tool.id),
                    data: json!({ "tool": tool.id, "timeout_ms": timeout.as_millis() }),
                    failure_classification: AgentTaskFailureClassification::Timeout,
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

fn apply_probe_environment(command: &mut Command, tool: &AgentTaskRuntimeTool) {
    command.env_clear().envs(&tool.env);
}

fn readiness_command_prefix(tool: &AgentTaskRuntimeTool) -> &[String] {
    tool.readiness
        .command_prefix
        .as_deref()
        .unwrap_or(&tool.command[1..])
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
