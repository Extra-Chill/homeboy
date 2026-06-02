use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskExecutorAdapter};
use crate::core::agent_task_secrets::{resolve_secret_env, AgentTaskSecretResolutionError};
use crate::core::extension::load_all_extensions;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProvider {
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub backend: String,
    pub command: String,
    pub request_schema: String,
    pub outcome_schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionProviderAgentTaskExecutor {
    providers: Vec<AgentTaskExecutorProvider>,
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self {
            providers: discover_agent_task_executor_providers(),
        }
    }

    #[cfg(test)]
    fn with_providers(providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self { providers }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }
}

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let Some(provider) = select_provider(&self.providers, &request) else {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.provider_missing",
                format!(
                    "no extension agent-task provider found for backend '{}'",
                    request.executor.backend
                ),
                json!({ "backend": request.executor.backend }),
            );
        };

        let missing_capabilities: Vec<String> = request
            .executor
            .required_capabilities
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect();
        if !missing_capabilities.is_empty() {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.capability_missing",
                format!(
                    "provider '{}' is missing required capabilities: {}",
                    provider.id,
                    missing_capabilities.join(", ")
                ),
                json!({ "provider": provider.id, "missing_capabilities": missing_capabilities }),
            );
        }

        run_provider_command(&request, provider)
    }
}

fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for manifest in load_all_extensions().unwrap_or_default() {
        let Some(value) = manifest.extra.get("agent_task_executors") else {
            continue;
        };
        let Ok(mut extension_providers) =
            serde_json::from_value::<Vec<AgentTaskExecutorProvider>>(value.clone())
        else {
            continue;
        };
        for provider in &mut extension_providers {
            provider.extension_id = Some(manifest.id.clone());
            provider.extension_path = manifest.extension_path.clone();
        }
        providers.extend(extension_providers);
    }
    providers
}

fn select_provider<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    request: &AgentTaskRequest,
) -> Option<&'a AgentTaskExecutorProvider> {
    providers.iter().find(|provider| {
        provider.backend == request.executor.backend
            && request
                .executor
                .selector
                .as_ref()
                .is_none_or(|selector| provider.id == *selector)
    })
}

fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let command = render_provider_command(provider);
    let Some((program, args)) = provider_command_parts(&command) else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_command_empty",
            format!("provider '{}' has an empty command", provider.id),
            json!({ "provider": provider.id }),
        );
    };
    let timeout = request
        .limits
        .timeout_ms
        .or(request.limits.max_runtime_ms)
        .map(Duration::from_millis);
    let input = match serde_json::to_vec(request) {
        Ok(input) => input,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.request_encode_failed",
                error.to_string(),
                json!({ "provider": provider.id }),
            )
        }
    };
    let env = match provider_command_env(request, provider) {
        Ok(env) => env,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.secret_env_missing",
                error.message,
                json!({ "provider": provider.id, "missing_secret_env": error.missing_secret_env }),
            )
        }
    };

    let mut child = match Command::new(&program)
        .args(&args)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_spawn_failed",
                error.to_string(),
                json!({ "provider": provider.id, "command": command }),
            )
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, &input);
    }

    let output = if let Some(timeout) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break child.wait_with_output(),
                Ok(None) if started.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return failure_outcome(
                        request,
                        AgentTaskOutcomeStatus::Timeout,
                        AgentTaskFailureClassification::Timeout,
                        "agent_task.provider_timeout",
                        format!(
                            "provider '{}' exceeded timeout_ms={}",
                            provider.id,
                            timeout.as_millis()
                        ),
                        json!({ "provider": provider.id, "command": command, "timeout_ms": timeout.as_millis() }),
                    );
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => break Err(error),
            }
        }
    } else {
        child.wait_with_output()
    };

    let Ok(output) = output else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_io_failed",
            "provider command failed while collecting output".to_string(),
            json!({ "provider": provider.id, "command": command }),
        );
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_empty_stdout",
            format!("provider '{}' produced no JSON outcome", provider.id),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr }),
        );
    }

    let parsed: Result<AgentTaskOutcome, _> = serde_json::from_str(&stdout);
    match parsed {
        Ok(mut outcome) => {
            if outcome.schema != AGENT_TASK_OUTCOME_SCHEMA {
                outcome.schema = AGENT_TASK_OUTCOME_SCHEMA.to_string();
            }
            outcome
        }
        Err(error) => failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_malformed_json",
            format!(
                "provider '{}' returned malformed JSON: {error}",
                provider.id
            ),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr, "stdout": stdout }),
        ),
    }
}

fn render_provider_command(provider: &AgentTaskExecutorProvider) -> String {
    let extension_path = provider.extension_path.as_deref().unwrap_or_default();
    provider
        .command
        .replace("{{extension_path}}", extension_path)
}

fn provider_command_parts(command: &str) -> Option<(String, Vec<String>)> {
    let mut parts = command.split_whitespace().map(str::to_string);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

fn provider_command_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    let mut env = vec![
        (
            "HOMEBOY_AGENT_TASK_PROVIDER_ID".to_string(),
            provider.id.clone(),
        ),
        (
            "HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON".to_string(),
            serde_json::to_string(&request.executor.config).unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_EXTENSION_ID".to_string(),
            provider.extension_id.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_EXTENSION_PATH".to_string(),
            provider.extension_path.clone().unwrap_or_default(),
        ),
    ];
    env.extend(resolve_secret_env(&request.executor.secret_env)?);
    Ok(env)
}

fn failure_outcome(
    request: &AgentTaskRequest,
    status: AgentTaskOutcomeStatus,
    classification: AgentTaskFailureClassification,
    diagnostic_class: &str,
    message: String,
    data: Value,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status,
        summary: Some(message.clone()),
        failure_classification: Some(classification),
        artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "agent-task-provider".to_string(),
            uri: format!("homeboy://agent-task/{}", diagnostic_class),
            label: Some("agent task provider dispatch".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: diagnostic_class.to_string(),
            message,
            data,
        }],
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_scheduler::{AgentTaskPlan, AgentTaskScheduler};
    use std::fs;

    fn script(body: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "homeboy-agent-task-provider-{}-{}.js",
            std::process::id(),
            body.len()
        ));
        fs::write(&path, body).expect("script written");
        path.to_string_lossy().to_string()
    }

    fn request(task_id: &str, command: String) -> (AgentTaskRequest, AgentTaskExecutorProvider) {
        let provider = AgentTaskExecutorProvider {
            schema: "homeboy/agent-task-executor-provider/v1".to_string(),
            id: "test.provider".to_string(),
            label: None,
            backend: "test".to_string(),
            command,
            request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            capabilities: vec!["structured_outcome".to_string()],
            extension_id: None,
            extension_path: None,
        };
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: Value::Null,
        };
        (request, provider)
    }

    #[test]
    fn scheduler_dispatches_extension_provider_command() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'ok'}));")
        );
        let (request, provider) = request("task-a", command);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-a", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Succeeded
        );
    }

    #[test]
    fn provider_timeout_returns_structured_outcome() {
        let command = format!("node {}", script("setInterval(() => {}, 1000);"));
        let (mut request, provider) = request("task-timeout", command);
        request.limits.timeout_ms = Some(50);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-timeout", vec![request]));

        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn provider_command_receives_executor_config_env() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let config=JSON.parse(process.env.HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:config.marker==='configured'?'succeeded':'failed',summary:process.env.HOMEBOY_AGENT_TASK_PROVIDER_ID}));")
        );
        let (mut request, mut provider) = request("task-config", command);
        request.executor.config = json!({ "marker": "configured" });
        provider.extension_id = Some("wordpress".to_string());
        provider.extension_path = Some("/tmp/homeboy-extension".to_string());
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-config", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].summary.as_deref(),
            Some("test.provider")
        );
    }

    #[test]
    fn provider_command_receives_declared_secret_env() {
        let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_SECRET_{}", std::process::id());
        std::env::set_var(&secret_name, "hydrated-secret");
        let command = format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:process.env.{secret_name}==='hydrated-secret'?'succeeded':'failed',summary:'checked'}}));"
            ))
        );
        let (mut request, provider) = request("task-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        std::env::remove_var(secret_name);
    }

    #[test]
    fn missing_declared_secret_env_fails_before_provider_spawn() {
        let secret_name = format!(
            "HOMEBOY_TEST_MISSING_AGENT_TASK_SECRET_{}",
            std::process::id()
        );
        std::env::remove_var(&secret_name);
        let command = format!(
            "node {}",
            script("throw new Error('provider should not run');")
        );
        let (mut request, provider) = request("task-missing-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-secret-env", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.secret_env_missing"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["missing_secret_env"],
            json!([secret_name])
        );
    }
}
