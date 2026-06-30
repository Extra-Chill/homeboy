use super::outcome_normalization::normalize_provider_outcome_roles;
use super::runner_readiness::{
    executable_file, provider_executable_env, resolve_executable_candidate,
};
use super::secrets::{provider_secret_env_plan_with_status, provider_secret_sources};
use super::*;
use crate::core::agent_task_executor_evidence::link_latest_executor_evidence;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProviderCommandEnvError {
    Secret(AgentTaskSecretResolutionError),
    Executable(AgentTaskProviderExecutableResolutionError),
}
/// Maximum number of attempts (1 initial + retries) for a transient provider
/// or network failure. Mirrors the bounded-retry pattern already used for
/// transient SSH failures (`server::client`) and SQLite-lock contention
/// (`observation::store`).
pub(super) const PROVIDER_TRANSIENT_MAX_ATTEMPTS: u32 = 3;

/// Base backoff between transient retries; doubles each attempt
/// (250ms, 500ms, ...). Keeps a single network blip from failing a whole cook
/// task without introducing unbounded delay.
const PROVIDER_TRANSIENT_BASE_BACKOFF_MS: u64 = 250;

/// Run the provider command with a bounded retry on transient provider/network
/// failures.
///
/// Transient failures (timeouts, connection resets, cURL error 28, 5xx,
/// temporarily-unavailable) are classified as retryable and retried with
/// escalating backoff. Permanent failures (auth, validation, malformed input,
/// capability gaps) fail fast on the first attempt. Each retry is surfaced in
/// the returned outcome diagnostics so the behaviour is visible in run output.
pub(super) fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let mut attempt = 1;
    loop {
        let mut outcome = run_provider_command_once(request, provider);
        classify_transient_provider_outcome(&mut outcome);

        let retryable = outcome_is_transient(&outcome);
        if !retryable || attempt >= PROVIDER_TRANSIENT_MAX_ATTEMPTS {
            if attempt > 1 {
                annotate_transient_retry(&mut outcome, attempt, retryable);
            }
            // Preserve and link the latest raw executor input/result as
            // first-class run evidence before returning the final outcome.
            link_latest_executor_evidence(request, &mut outcome);
            return outcome;
        }

        let backoff_ms = PROVIDER_TRANSIENT_BASE_BACKOFF_MS.saturating_mul(1u64 << (attempt - 1));
        if backoff_ms > 0 {
            std::thread::sleep(Duration::from_millis(backoff_ms));
        }
        attempt += 1;
    }
}

/// True when an outcome represents a transient provider/network failure that is
/// safe to retry.
fn outcome_is_transient(outcome: &AgentTaskOutcome) -> bool {
    outcome.failure_classification == Some(AgentTaskFailureClassification::Transient)
}

/// Promote a `ProviderError`/`Provider` outcome to the `Transient`
/// classification when its surfaced text looks like a transient network or
/// provider blip. Leaves permanent provider failures untouched so they keep
/// failing fast.
fn classify_transient_provider_outcome(outcome: &mut AgentTaskOutcome) {
    let already_transient =
        outcome.failure_classification == Some(AgentTaskFailureClassification::Transient);
    let provider_failure = matches!(
        outcome.status,
        AgentTaskOutcomeStatus::ProviderError | AgentTaskOutcomeStatus::Failed
    ) && matches!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider) | None
    );

    if already_transient {
        return;
    }
    if !provider_failure {
        return;
    }

    if outcome_text_is_transient(outcome) {
        outcome.failure_classification = Some(AgentTaskFailureClassification::Transient);
    }
}

/// Gather the human-facing text of an outcome (summary, diagnostic messages,
/// diagnostic data) and check it for transient-failure signatures.
fn outcome_text_is_transient(outcome: &AgentTaskOutcome) -> bool {
    if let Some(summary) = outcome.summary.as_deref() {
        if is_transient_provider_error(summary) {
            return true;
        }
    }
    for diagnostic in &outcome.diagnostics {
        if is_transient_provider_error(&diagnostic.message) {
            return true;
        }
        if is_transient_provider_error(&diagnostic.data.to_string()) {
            return true;
        }
    }
    false
}

/// Classify provider/network error text as transient (retryable) vs permanent.
///
/// Mirrors `server::client::is_transient_ssh_error`: matches on a curated set
/// of substrings that indicate a transient blip rather than a deterministic
/// failure. Matching is case-insensitive.
pub(super) fn is_transient_provider_error(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    const TRANSIENT_PATTERNS: [&str; 16] = [
        "curl error 28",
        "operation timed out",
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "connection closed",
        "broken pipe",
        "network error",
        "network is unreachable",
        "temporary failure",
        "temporarily unavailable",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "too many requests",
    ];

    if TRANSIENT_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return true;
    }

    // HTTP 5xx and 429 status codes are transient; 4xx (except 429) are not.
    transient_status_code(&lowered)
}

/// Detect a transient HTTP status code (5xx or 429) mentioned in error text,
/// while leaving permanent 4xx codes (400/401/403/404/422) non-retryable.
fn transient_status_code(lowered: &str) -> bool {
    const TRANSIENT_CODES: [&str; 7] = ["429", "500", "502", "503", "504", "522", "524"];
    TRANSIENT_CODES
        .iter()
        .any(|code| contains_status_code_token(lowered, code))
}

fn contains_status_code_token(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + code.len()..].chars().next();
        !before.is_some_and(|ch| ch.is_ascii_alphanumeric())
            && !after.is_some_and(|ch| ch.is_ascii_alphanumeric())
    })
}

/// Record the transient retry history on the final outcome so operators can see
/// that a cook task recovered from (or exhausted retries on) a transient blip.
fn annotate_transient_retry(outcome: &mut AgentTaskOutcome, attempts: u32, exhausted: bool) {
    let message = if exhausted {
        format!(
            "transient provider/network failure persisted after {attempts} attempt(s); retries exhausted"
        )
    } else {
        format!(
            "recovered after retrying transient provider/network failure ({attempts} attempt(s))"
        )
    };
    outcome.diagnostics.push(AgentTaskDiagnostic {
        class: "agent_task.provider_transient_retry".to_string(),
        message,
        data: json!({ "attempts": attempts, "retries_exhausted": exhausted }),
    });
}

pub(super) fn run_provider_command_once(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let command = render_provider_command_display(provider);
    let Some((program, args, cwd)) = provider_command_parts(provider) else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_command_empty",
            format!("provider '{}' has an empty command", provider.id),
            json!({ "provider": provider.id }),
        );
    };

    if let Some(preflight) = provider_preflight_failure(request, provider, &program, &cwd, &command)
    {
        return preflight;
    }

    let timeout = request
        .limits
        .timeout_ms
        .or(request.limits.max_runtime_ms)
        .map(|timeout_ms| (timeout_ms, timeout_with_grace(timeout_ms)));
    let mut provider_request = request.clone();
    provider_request.normalize_artifact_declarations();
    let input = match serde_json::to_vec(&provider_request) {
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
        Err(ProviderCommandEnvError::Secret(error)) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.secret_env_missing",
                error.message,
                json!({ "provider": provider.id, "missing_secret_env": error.missing_secret_env }),
            )
        }
        Err(ProviderCommandEnvError::Executable(error)) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_executable_missing",
                error.message(),
                json!({
                    "provider": provider.id,
                    "readiness_id": error.readiness_id,
                    "env": error.env,
                    "candidates": error.candidates,
                    "install_hint": error.install_hint,
                }),
            )
        }
    };

    let mut command_builder = Command::new(&program);
    command_builder.args(&args).envs(
        env.iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    if let Some(cwd) = cwd {
        command_builder.current_dir(cwd);
    }

    let mut child = match command_builder
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

    let output = if let Some((requested_timeout_ms, process_timeout)) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break child.wait_with_output(),
                Ok(None) if started.elapsed() >= process_timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return failure_outcome(
                        request,
                        AgentTaskOutcomeStatus::Timeout,
                        AgentTaskFailureClassification::Timeout,
                        "agent_task.provider_timeout",
                        format!(
                            "provider '{}' exceeded timeout_ms={}",
                            provider.id, requested_timeout_ms
                        ),
                        json!({ "provider": provider.id, "command": command, "timeout_ms": requested_timeout_ms, "process_timeout_ms": process_timeout.as_millis() }),
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
        if let Some(mut outcome) = parse_provider_outcome_from_mixed_output(&stderr) {
            normalize_provider_outcome_roles(&mut outcome, provider);
            return outcome;
        }
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
            normalize_provider_outcome_roles(&mut outcome, provider);
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

fn provider_preflight_failure(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    program: &str,
    cwd: &Option<PathBuf>,
    command: &str,
) -> Option<AgentTaskOutcome> {
    let digest = provider_preflight_digest(request, provider, program, cwd, command);
    if digest.failures.is_empty() {
        return None;
    }

    Some(failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        digest.classification,
        digest.diagnostic_class,
        digest.message,
        digest.data,
    ))
}

struct ProviderPreflightDigest {
    diagnostic_class: &'static str,
    classification: AgentTaskFailureClassification,
    message: String,
    data: Value,
    failures: Vec<Value>,
}

fn provider_preflight_digest(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    program: &str,
    cwd: &Option<PathBuf>,
    command: &str,
) -> ProviderPreflightDigest {
    let mut failures = Vec::new();
    let mut diagnostic_class = "agent_task.provider_preflight_failed";
    let mut classification = AgentTaskFailureClassification::Provider;

    if !provider_command_program_available(program) {
        diagnostic_class = "agent_task.provider_command_unavailable";
        failures.push(json!({
            "field": "command",
            "message": format!("provider command executable '{program}' is not available"),
            "remediation": format!("Install '{program}' on the runner or configure the provider invocation with an absolute executable path available to the runner PATH."),
        }));
    }

    if let Some(cwd) = cwd {
        if !cwd.is_dir() {
            failures.push(json!({
                "field": "invocation.cwd",
                "message": format!("provider command working directory '{}' does not exist", cwd.display()),
                "remediation": "Fix the provider runtime path or invocation.cwd template so it resolves to an existing directory on the runner.",
            }));
        }
    }

    let secret_status = provider_secret_env_plan_with_status(provider, request).status;
    let missing_secret_env: Vec<String> = secret_status
        .iter()
        .filter(|status| !status.configured)
        .map(|status| status.name.clone())
        .collect();
    if !missing_secret_env.is_empty() {
        diagnostic_class = "agent_task.secret_env_missing";
        classification = AgentTaskFailureClassification::InvalidInput;
        failures.push(json!({
            "field": "secret_env",
            "message": format!("missing provider secret env: {}", missing_secret_env.join(", ")),
            "remediation": "Set the missing secret_env values in the runner environment or Homeboy secret-env configuration before launching the sandbox.",
        }));
    }

    let message = if failures.len() == 1 {
        failures[0]
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("agent-task provider preflight failed")
            .to_string()
    } else {
        format!(
            "agent-task provider preflight failed with {} actionable issue(s)",
            failures.len()
        )
    };

    let digest_failures = failures.clone();
    let data = json!({
        "provider": provider.id,
        "backend": provider.backend,
        "command": command,
        "program": program,
        "path": std::env::var_os("PATH").map(|value| value.to_string_lossy().to_string()).unwrap_or_default(),
        "runtime_path_provenance": runtime_path_provenance(provider),
        "missing_secret_env": missing_secret_env,
        "secret_env_status": secret_status,
        "failures": failures,
    });

    ProviderPreflightDigest {
        diagnostic_class,
        classification,
        message,
        data,
        failures: digest_failures,
    }
}

fn provider_command_program_available(program: &str) -> bool {
    let program = program.trim();
    if program.is_empty() {
        return false;
    }
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return executable_file(path);
    }
    resolve_executable_candidate(program).is_some()
}

fn runtime_path_provenance(provider: &AgentTaskExecutorProvider) -> Value {
    let (path, source) = if let Some(runtime_path) = provider.runtime_path.as_deref() {
        (runtime_path, "runtime_path")
    } else if let Some(extension_path) = provider.extension_path.as_deref() {
        (extension_path, "extension_path_fallback")
    } else {
        ("", "missing")
    };
    json!({
        "runtime_id": provider.runtime_id.as_deref(),
        "runtime_path": path,
        "source": source,
        "extension_id": provider.extension_id.as_deref(),
        "extension_path": provider.extension_path.as_deref(),
    })
}

fn parse_provider_outcome_from_mixed_output(output: &str) -> Option<AgentTaskOutcome> {
    if output.trim().is_empty() {
        return None;
    }
    if let Ok(outcome) = serde_json::from_str::<AgentTaskOutcome>(output) {
        return Some(outcome);
    }

    for (index, _) in output.match_indices('{') {
        let mut stream =
            serde_json::Deserializer::from_str(&output[index..]).into_iter::<AgentTaskOutcome>();
        if let Some(Ok(outcome)) = stream.next() {
            return Some(outcome);
        }
    }
    None
}
pub(super) fn render_provider_command_display(provider: &AgentTaskExecutorProvider) -> String {
    if let Some(display) = provider.invocation.display.as_deref() {
        return render_provider_command_template(display, provider);
    }
    if !provider.invocation.argv.is_empty() {
        return render_provider_invocation_argv(provider).join(" ");
    }
    if !provider.command_argv.is_empty() {
        return render_provider_command_argv(provider).join(" ");
    }

    render_provider_command_string(provider)
}

fn render_provider_command_string(provider: &AgentTaskExecutorProvider) -> String {
    render_provider_command_template(&provider.command, provider)
}

fn render_provider_command_template(value: &str, provider: &AgentTaskExecutorProvider) -> String {
    let extension_path = provider.extension_path.as_deref().unwrap_or_default();
    let runtime_path = provider.runtime_path.as_deref().unwrap_or(extension_path);
    value
        .replace("{{extension_path}}", extension_path)
        .replace("{{runtime_path}}", runtime_path)
}

fn render_provider_command_argv(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    provider
        .command_argv
        .iter()
        .map(|arg| render_provider_command_template(arg, provider))
        .collect()
}

fn render_provider_invocation_argv(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    provider
        .invocation
        .argv
        .iter()
        .map(|arg| render_provider_command_template(arg, provider))
        .collect()
}

pub(super) fn provider_command_parts(
    provider: &AgentTaskExecutorProvider,
) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    let (argv, cwd) = if !provider.invocation.argv.is_empty() {
        (
            render_provider_invocation_argv(provider),
            provider
                .invocation
                .cwd
                .as_deref()
                .map(|cwd| PathBuf::from(render_provider_command_template(cwd, provider))),
        )
    } else if provider.command_argv.is_empty() {
        // Legacy string commands retain their historical split behavior for
        // compatibility. New provider manifests should use command_argv/argv.
        eprintln!(
            "Warning: agent task provider '{}' uses deprecated string command; use invocation.argv or argv instead",
            provider.id
        );
        (
            render_provider_command_string(provider)
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            None,
        )
    } else {
        (render_provider_command_argv(provider), None)
    };
    let mut parts = argv.into_iter();
    let program = parts.next()?;
    Some((program, parts.collect(), cwd))
}

pub(super) fn provider_command_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, ProviderCommandEnvError> {
    // Both runtime path env vars resolve to the provider runtime_path, falling
    // back to the extension_path when the runtime is not separately declared.
    let runtime_path = provider
        .runtime_path
        .clone()
        .or_else(|| provider.extension_path.clone())
        .unwrap_or_default();
    let secret_env_plan = provider_secret_env_plan_with_status(provider, request);
    let mut env = vec![
        (
            "HOMEBOY_AGENT_TASK_PROVIDER_ID".to_string(),
            provider.id.clone(),
        ),
        (
            "HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON".to_string(),
            serde_json::to_string(&request.executor.config).unwrap_or_else(|_| "null".to_string()),
        ),
        secret_env_plan.json_env_pair(),
        (
            "HOMEBOY_AGENT_TOOL_POLICY_JSON".to_string(),
            serde_json::to_string(&request.policy.tools).unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA".to_string(),
            AGENT_TOOL_REQUEST_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_RESULT_SCHEMA".to_string(),
            AGENT_TOOL_RESULT_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_POLICY_SCHEMA".to_string(),
            AGENT_TOOL_POLICY_SCHEMA.to_string(),
        ),
        (
            "HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND".to_string(),
            agent_tool_dispatch_command(),
        ),
        (
            "HOMEBOY_EXTENSION_ID".to_string(),
            provider.extension_id.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_EXTENSION_PATH".to_string(),
            provider.extension_path.clone().unwrap_or_default(),
        ),
        ("HOMEBOY_RUNTIME_PATH".to_string(), runtime_path.clone()),
        (
            "HOMEBOY_AGENT_RUNTIME_ID".to_string(),
            provider.runtime_id.clone().unwrap_or_default(),
        ),
        ("HOMEBOY_AGENT_RUNTIME_PATH".to_string(), runtime_path),
    ];
    env.extend(provider_executable_env(provider).map_err(ProviderCommandEnvError::Executable)?);
    env.extend(
        resolve_secret_env_with_fallbacks(
            &request.executor.secret_env,
            &provider_secret_sources(provider, Some(request)),
        )
        .map_err(ProviderCommandEnvError::Secret)?,
    );
    Ok(env)
}

fn agent_tool_dispatch_command() -> String {
    let current_exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .expect("current executable path is required for agent tool dispatch command");
    format!(
        "{} agent-task tool dispatch",
        shell::quote_arg(&current_exe)
    )
}

pub(super) fn failure_outcome(
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
        typed_artifacts: Vec::new(),
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
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}
