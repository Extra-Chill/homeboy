use super::artifact_finalization::finalize_provider_file_artifacts;
use super::outcome_normalization::{normalize_provider_outcome_roles, push_unique_diagnostic};
use super::runner_readiness::{
    executable_file, provider_executable_env, resolve_executable_candidate,
};
use super::secrets::{provider_secret_env_plan_with_status, provider_secret_sources};
use super::*;
use crate::core::agent_task_executor_evidence::link_latest_executor_evidence;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 16 * 1024;
const REDACTED_VALUE: &str = "[redacted]";

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
pub(super) fn run_materialized_provider_command(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    run_id: Option<&str>,
) -> AgentTaskOutcome {
    let mut attempt = 1;
    loop {
        let mut outcome = run_materialized_provider_command_once(request, provider);
        classify_transient_provider_outcome(&mut outcome);

        let retryable = outcome_is_transient(&outcome);
        if !retryable || attempt >= PROVIDER_TRANSIENT_MAX_ATTEMPTS {
            if attempt > 1 {
                annotate_transient_retry(&mut outcome, attempt, retryable);
            }
            // Preserve and link the latest raw executor input/result as
            // first-class run evidence before returning the final outcome.
            link_latest_executor_evidence(request, &mut outcome, run_id);
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
    if outcome_text_is_rate_limited(outcome) {
        outcome.failure_classification = Some(AgentTaskFailureClassification::RateLimited);
        return;
    }
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

fn outcome_text_is_rate_limited(outcome: &AgentTaskOutcome) -> bool {
    outcome
        .summary
        .as_deref()
        .is_some_and(is_rate_limited_provider_error)
        || outcome.diagnostics.iter().any(|diagnostic| {
            is_rate_limited_provider_error(&diagnostic.message)
                || is_rate_limited_provider_error(&diagnostic.data.to_string())
        })
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
    const TRANSIENT_PATTERNS: [&str; 15] = [
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
    ];

    if TRANSIENT_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return true;
    }

    // HTTP 5xx status codes are transient; rate-limit 429 is distinct so the
    // scheduler can rotate rather than retry the same throttled provider.
    transient_status_code(&lowered)
}

pub(super) fn is_rate_limited_provider_error(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    [
        "too many requests",
        "rate limit",
        "rate-limit",
        "provider_quota",
        "provider quota",
        "quota exceeded",
        "exceeded your quota",
    ]
    .iter()
    .any(|pattern| lowered.contains(pattern))
        || contains_status_code_token(&lowered, "429")
}

/// Detect a transient HTTP 5xx status code mentioned in error text, while
/// leaving permanent 4xx codes and rate-limit 429 non-retryable here.
fn transient_status_code(lowered: &str) -> bool {
    const TRANSIENT_CODES: [&str; 6] = ["500", "502", "503", "504", "522", "524"];
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

pub(super) fn run_materialized_provider_command_once(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let command = render_provider_command_display(provider);
    let Some((program, args, provider_cwd)) = provider_command_parts(provider) else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_command_empty",
            format!("provider '{}' has an empty command", provider.id),
            json!({ "provider": provider.id }),
        );
    };

    // The scheduler may replace a task workspace with an isolated attempt
    // worktree. That task root is the execution cwd; a manifest cwd is only a
    // fallback for requests without a workspace.
    let cwd = request
        .request
        .workspace
        .root
        .as_deref()
        .map(PathBuf::from)
        .or(provider_cwd);

    if let Some(preflight) = provider_preflight_failure(request, provider, &program, &cwd, &command)
    {
        return preflight;
    }

    let requested_timeout_ms = crate::core::agent_task_timeout::effective_provider_timeout_ms(
        request.limits.timeout_ms,
        request.limits.max_runtime_ms,
    );
    let process_timeout = timeout_with_grace(requested_timeout_ms);
    let mut provider_request = request.clone();
    provider_request.request.limits.timeout_ms = Some(requested_timeout_ms);
    provider_request.request.normalize_artifact_declarations();
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

    let stdout_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let last_progress: Arc<AtomicU64> = Arc::new(AtomicU64::new(now_unix_ms()));

    let stdout_reader = child.stdout.take().map(|stdout| {
        spawn_output_reader(
            stdout,
            Arc::clone(&stdout_buffer),
            Arc::clone(&last_progress),
        )
    });
    let stderr_reader = child.stderr.take().map(|stderr| {
        spawn_output_reader(
            stderr,
            Arc::clone(&stderr_buffer),
            Arc::clone(&last_progress),
        )
    });

    if let Some(mut stdin) = child.stdin.take() {
        let _ = Write::write_all(&mut stdin, &input);
    }

    let started = Instant::now();
    let liveness_timeout = request
        .limits
        .liveness_timeout_ms
        .map(Duration::from_millis);
    let (status, killed_for_liveness, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false, false),
            Ok(None) => {
                let elapsed = started.elapsed();
                if elapsed >= process_timeout {
                    break (None, false, true);
                }
                if let Some(liveness) = liveness_timeout {
                    let progress_age = Duration::from_millis(
                        now_unix_ms().saturating_sub(last_progress.load(Ordering::SeqCst)),
                    );
                    if progress_age >= liveness {
                        break (None, true, false);
                    }
                    // Wake up at the earlier of process timeout and liveness deadline.
                    let remaining_liveness = liveness.saturating_sub(progress_age);
                    let sleep_for = remaining_liveness
                        .min(process_timeout - elapsed)
                        .min(Duration::from_millis(50));
                    if sleep_for > Duration::ZERO {
                        std::thread::sleep(sleep_for);
                    }
                    continue;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break (None, false, false),
        }
    };

    if killed_for_liveness || timed_out {
        let _ = child.kill();
        let _ = child.wait();
    }

    if let Some(handle) = stdout_reader {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_reader {
        let _ = handle.join();
    }

    let stdout_bytes = stdout_buffer.lock().expect("stdout buffer").clone();
    let stderr_bytes = stderr_buffer.lock().expect("stderr buffer").clone();
    let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();

    if killed_for_liveness {
        let (status, classification, message) =
            classify_stall_or_rate_limit(&stdout, &stderr, &provider.id, requested_timeout_ms);
        return failure_outcome(
            request,
            status,
            classification,
            "agent_task.provider_liveness_timeout",
            message,
            json!({
                "provider": provider.id,
                "command": command,
                "timeout_ms": requested_timeout_ms,
                "process_timeout_ms": process_timeout.as_millis(),
                "liveness_timeout_ms": request.limits.liveness_timeout_ms,
                "stdout_bytes": stdout_bytes.len(),
                "stderr_bytes": stderr_bytes.len(),
            }),
        );
    }

    if timed_out {
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
    let Some(status) = status else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_io_failed",
            "provider command failed while collecting output".to_string(),
            json!({ "provider": provider.id, "command": command }),
        );
    };
    if stdout.is_empty() {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_empty_stdout",
            format!("provider '{}' produced no JSON outcome", provider.id),
            executor_process_diagnostic_data(
                &provider.id,
                &provider.backend,
                &command,
                &status,
                &stdout,
                &stderr,
                &provider_output_redactions(request, provider),
            ),
        );
    }

    let parsed: Result<AgentTaskOutcome, _> = serde_json::from_str(&stdout);
    match parsed {
        Ok(mut outcome) => {
            if outcome.schema != AGENT_TASK_OUTCOME_SCHEMA {
                outcome.schema = AGENT_TASK_OUTCOME_SCHEMA.to_string();
            }
            normalize_provider_outcome_roles(&mut outcome, provider);
            if let Err(error) =
                finalize_provider_file_artifacts(&mut outcome, &request.artifacts_root_identity)
            {
                return failure_outcome(
                    request,
                    AgentTaskOutcomeStatus::Failed,
                    AgentTaskFailureClassification::InvalidInput,
                    "agent_task.artifact_finalization_failed",
                    error.message,
                    json!({ "provider": provider.id, "details": error.details }),
                );
            }
            surface_provider_process_failure(
                &mut outcome,
                request,
                provider,
                &command,
                &status,
                &stdout,
                &stderr,
            );
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
            executor_process_diagnostic_data(
                &provider.id,
                &provider.backend,
                &command,
                &status,
                &stdout,
                &stderr,
                &provider_output_redactions(request, provider),
            ),
        ),
    }
}

#[cfg(test)]
pub(super) fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    run_id: Option<&str>,
) -> AgentTaskOutcome {
    let materialized = test_executor_request(request);
    run_materialized_provider_command(&materialized, provider, run_id)
}

#[cfg(test)]
pub(super) fn run_provider_command_once(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let materialized = test_executor_request(request);
    run_materialized_provider_command_once(&materialized, provider)
}

#[cfg(test)]
fn test_executor_request(request: &AgentTaskRequest) -> AgentTaskExecutorRequest {
    let artifacts_path = std::env::temp_dir()
        .join("homeboy-agent-task-provider-tests")
        .join(crate::core::paths::sanitize_path_segment(&request.task_id))
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&artifacts_path).expect("test executor artifact root");
    AgentTaskExecutorRequest {
        request: request.clone(),
        artifacts_root_identity: crate::core::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture(&artifacts_path).expect("test artifact identity"),
        artifacts_path,
        artifacts_path_provenance: AgentTaskArtifactsPathProvenance {
            owner: "homeboy".to_string(),
            locality: "runner".to_string(),
            plan_id: "provider-unit-test".to_string(),
            run_id: None,
            task_id: request.task_id.clone(),
            attempt: 1,
        },
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn spawn_output_reader<R>(
    mut reader: R,
    buffer: Arc<Mutex<Vec<u8>>>,
    last_progress: Arc<AtomicU64>,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut chunk = [0; 4096];
        loop {
            let Ok(read) = reader.read(&mut chunk) else {
                break;
            };
            if read == 0 {
                break;
            }
            buffer.lock().expect("output buffer").extend(&chunk[..read]);
            last_progress.store(now_unix_ms(), Ordering::SeqCst);
        }
    })
}

fn classify_stall_or_rate_limit(
    stdout: &str,
    stderr: &str,
    provider_id: &str,
    requested_timeout_ms: u64,
) -> (
    AgentTaskOutcomeStatus,
    AgentTaskFailureClassification,
    String,
) {
    let output = format!("{stdout}\n{stderr}");
    if is_rate_limited_provider_error(&output) {
        return (
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::RateLimited,
            format!("provider '{provider_id}' reported a rate limit before becoming unresponsive"),
        );
    }
    (
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Stalled,
        format!(
            "provider '{provider_id}' produced no stdout/stderr progress before timeout_ms={requested_timeout_ms}"
        ),
    )
}

fn executor_process_diagnostic_data(
    provider_id: &str,
    provider_backend: &str,
    command: &str,
    status: &std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
    redactions: &[String],
) -> Value {
    let command = redact_sensitive_text(command, redactions);
    let stdout = redact_sensitive_text(stdout, redactions);
    let stderr = redact_sensitive_text(stderr, redactions);
    json!({
        "provider": provider_id,
        "provider_backend": provider_backend,
        "command": command,
        "exit_code": status.code(),
        "signal": exit_signal(status),
        "stdout": bounded_executor_output(&stdout),
        "stdout_bytes": stdout.len(),
        "stdout_truncated": stdout.len() > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES,
        "stderr": bounded_executor_output(&stderr),
        "stderr_bytes": stderr.len(),
        "stderr_truncated": stderr.len() > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES,
        "remediation_hints": provider_process_remediation_hints(&stdout, &stderr),
    })
}

fn surface_provider_process_failure(
    outcome: &mut AgentTaskOutcome,
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    command: &str,
    status: &std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
) {
    if status.success() {
        return;
    }

    if outcome.status == AgentTaskOutcomeStatus::Succeeded {
        outcome.status = AgentTaskOutcomeStatus::ProviderError;
        outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
    }

    let redactions = provider_output_redactions(request, provider);
    let data = executor_process_diagnostic_data(
        &provider.id,
        &provider.backend,
        command,
        status,
        stdout,
        stderr,
        &redactions,
    );
    let exit_description = status
        .code()
        .map(|code| format!("status {code}"))
        .or_else(|| exit_signal(status).map(|signal| format!("signal {signal}")))
        .unwrap_or_else(|| "unknown status".to_string());
    let stderr_tail = data
        .get("stderr")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let message = match stderr_tail {
        Some(stderr_tail) => format!(
            "provider '{}' ({}) exited with {exit_description}: {stderr_tail}",
            provider.id, provider.backend
        ),
        None => format!(
            "provider '{}' ({}) exited with {exit_description}; inspect stdout/stderr diagnostics",
            provider.id, provider.backend
        ),
    };

    push_unique_diagnostic(
        &mut outcome.diagnostics,
        "agent_task.provider_process_failed".to_string(),
        message,
        data,
    );
}

fn bounded_executor_output(output: &str) -> String {
    if output.len() <= EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES {
        return output.to_string();
    }

    let mut start = output.len() - EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].to_string()
}

fn provider_output_redactions(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    names.extend(request.executor.secret_env.iter().cloned());
    names.extend(provider.invocation.redaction.env.iter().cloned());
    for env_ref in &provider.invocation.env {
        if env_ref.redacted.unwrap_or(false) {
            names.insert(env_ref.name.clone());
        }
    }
    for requirement in &provider.secret_requirements {
        names.extend(requirement.env.iter().cloned());
    }
    for requirement in &provider.secret_env_requirements {
        names.extend(requirement.env.iter().cloned());
    }
    for readiness in &provider.runner_readiness {
        names.extend(readiness.secret_env.iter().cloned());
        if let Some(executable) = &readiness.executable {
            names.extend(executable.env.iter().cloned());
        }
    }

    names
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| value.len() >= 4)
        .collect()
}

fn redact_sensitive_text<'a>(text: &'a str, redactions: &[String]) -> std::borrow::Cow<'a, str> {
    let mut redacted = std::borrow::Cow::Borrowed(text);
    for value in redactions {
        if value.is_empty() || !redacted.contains(value) {
            continue;
        }
        redacted = std::borrow::Cow::Owned(redacted.replace(value, REDACTED_VALUE));
    }
    redacted
}

fn provider_process_remediation_hints(stdout: &str, stderr: &str) -> Vec<String> {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    let mut hints = Vec::new();
    if combined.contains("auth")
        || combined.contains("unauthorized")
        || combined.contains("permission denied")
        || combined.contains("forbidden")
        || combined.contains("api key")
        || combined.contains("token")
    {
        hints.push(
            "Check provider authentication and required secret_env values on the runner."
                .to_string(),
        );
    }
    if combined.contains("timeout") || combined.contains("timed out") {
        hints.push("Retry after the provider is reachable, or increase the task timeout when the operation is expected to run longer.".to_string());
    }
    if combined.contains("not found") || combined.contains("no such file") {
        hints.push(
            "Verify the provider executable, runtime path, and working directory on the runner."
                .to_string(),
        );
    }
    hints.push("Inspect the bounded stdout/stderr tails in this diagnostic before retrying the agent-task run.".to_string());
    hints
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;

    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
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
pub(crate) fn render_provider_command_display(provider: &AgentTaskExecutorProvider) -> String {
    if let Some(display) = provider.invocation.display.as_deref() {
        return render_provider_command_template(display, provider);
    }
    if !provider.invocation.argv.is_empty() {
        return render_provider_invocation_argv(provider).join(" ");
    }
    if !provider.command_argv.is_empty() {
        return render_provider_command_argv(provider).join(" ");
    }

    String::new()
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

pub(crate) fn provider_command_parts(
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
    } else {
        (render_provider_command_argv(provider), None)
    };
    let mut parts = argv.into_iter();
    let program = parts.next()?;
    Some((program, parts.collect(), cwd))
}

/// Result of probing whether a provider's executor entrypoint actually loads on
/// disk — i.e. its module require graph resolves against the materialized
/// runtime layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderExecutorResolution {
    /// The executor entrypoint loaded and printed its provider contract; the
    /// require graph resolves.
    Resolved,
    /// The probe could not be run (no command parts, or the invocation is not a
    /// runtime we can safely dry-load). Not a failure — nothing to assert.
    Skipped { reason: String },
    /// The executor entrypoint failed to load: its require graph does not
    /// resolve on disk (e.g. a shared runtime package was never materialized).
    Unresolved { command: String, detail: String },
}

/// Grace window for the `--provider-contract` dry load. This only parses the
/// executor module and prints a static contract, so it returns near-instantly;
/// the timeout only guards against a pathological hang.
const EXECUTOR_RESOLUTION_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Probe whether a provider's executor entrypoint resolves its module require
/// graph on disk, without executing an agent task.
///
/// Every CLI-runtime executor wrapper resolves its full `require()` chain at
/// module load (top-level requires run before any argument handling) and
/// implements a `--provider-contract` flag that prints the static provider
/// contract and exits 0 *before* reading any request from stdin. Invoking the
/// wrapper with `--provider-contract` and closed stdin therefore forces the
/// entire require graph to load: if a shared runtime package (e.g.
/// `agent-task-contracts`) was never materialized next to the runtime, Node
/// aborts at load with `MODULE_NOT_FOUND` and exits non-zero — exactly the
/// failure that would otherwise only surface mid-cook as empty provider stdout.
///
/// This is the on-disk resolution check the doctor readiness verdict was
/// missing (Extra-Chill/homeboy#7736): provider *contract* discovery reads
/// declared metadata and never loads the executor, so a partially-materialized
/// install passed readiness while every cook crashed.
pub(crate) fn probe_provider_executor_resolves(
    provider: &AgentTaskExecutorProvider,
) -> ProviderExecutorResolution {
    let command = render_provider_command_display(provider);
    let Some((program, args, cwd)) = provider_command_parts(provider) else {
        return ProviderExecutorResolution::Skipped {
            reason: format!("provider '{}' has no resolvable command", provider.id),
        };
    };

    // Only node-runtime executors implement the `--provider-contract` dry-load
    // contract. Other runtimes are skipped rather than probed with a flag they
    // do not understand (which could block reading stdin). Core stays
    // runtime-agnostic: it keys off the resolved program basename, not a
    // hard-coded ecosystem/provider name.
    let program_name = Path::new(&program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program.as_str());
    let is_node_runtime = program_name == "node" || program_name == "nodejs";
    if !is_node_runtime {
        return ProviderExecutorResolution::Skipped {
            reason: format!(
                "provider '{}' executor program '{program_name}' does not implement the --provider-contract dry-load probe",
                provider.id
            ),
        };
    }

    let mut probe_args = args.clone();
    probe_args.push("--provider-contract".to_string());

    let mut command_builder = Command::new(&program);
    command_builder
        .args(&probe_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command_builder.current_dir(cwd);
    }

    let mut child = match command_builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProviderExecutorResolution::Unresolved {
                command: command.clone(),
                detail: format!("failed to spawn executor probe: {error}"),
            };
        }
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= EXECUTOR_RESOLUTION_PROBE_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProviderExecutorResolution::Unresolved {
                        command,
                        detail: "executor resolution probe timed out".to_string(),
                    };
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                return ProviderExecutorResolution::Unresolved {
                    command,
                    detail: format!("executor resolution probe wait failed: {error}"),
                };
            }
        }
    };

    if status.success() {
        return ProviderExecutorResolution::Resolved;
    }

    let stderr = child
        .stderr
        .take()
        .map(|mut stderr| {
            let mut buffer = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stderr, &mut buffer);
            String::from_utf8_lossy(&buffer).trim().to_string()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            "executor exited non-zero while loading its module require graph".to_string()
        });

    ProviderExecutorResolution::Unresolved {
        command,
        detail: first_stderr_lines(&stderr, 8),
    }
}

/// Keep the first `max` lines of captured stderr so the blocker carries the
/// actionable `MODULE_NOT_FOUND` / require-stack context without dumping an
/// unbounded trace.
fn first_stderr_lines(stderr: &str, max: usize) -> String {
    stderr.lines().take(max).collect::<Vec<_>>().join("\n")
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

#[cfg(test)]
mod executor_resolution_tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal node-runtime provider whose invocation runs the given
    /// script path under `node`, mirroring how installed CLI executor wrappers
    /// are invoked (`node <wrapper>.cjs`).
    fn node_provider(script: &std::path::Path) -> AgentTaskExecutorProvider {
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.node.provider",
            "backend": "test",
            "invocation": {
                "argv": ["node", script.display().to_string()],
            },
        }))
        .expect("provider parses");
        // Ensure no legacy string command path is taken.
        provider.command.clear();
        provider
    }

    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("create script");
        file.write_all(body.as_bytes()).expect("write script");
        path
    }

    #[test]
    fn resolved_when_provider_contract_dry_load_exits_zero() {
        let dir = tempfile::tempdir().expect("dir");
        // Emulates a healthy executor wrapper: handles --provider-contract
        // before reading stdin, after resolving its (here trivial) require graph.
        let script = write_script(
            dir.path(),
            "healthy-executor.cjs",
            r#"if (process.argv.includes('--provider-contract')) {
  process.stdout.write(JSON.stringify({ id: 'test.node.provider' }));
  process.exit(0);
}
process.exit(2);
"#,
        );
        let provider = node_provider(&script);

        assert_eq!(
            probe_provider_executor_resolves(&provider),
            ProviderExecutorResolution::Resolved
        );
    }

    #[test]
    fn unresolved_when_require_graph_is_broken() {
        let dir = tempfile::tempdir().expect("dir");
        // Emulates the #7736 failure: a top-level require of a runtime package
        // that was never materialized. Node aborts at module load with
        // MODULE_NOT_FOUND before any argument handling runs.
        let script = write_script(
            dir.path(),
            "broken-executor.cjs",
            "require('./this-shared-runtime-package-was-never-materialized');\n",
        );
        let provider = node_provider(&script);

        match probe_provider_executor_resolves(&provider) {
            ProviderExecutorResolution::Unresolved { detail, .. } => {
                assert!(
                    detail.contains("MODULE_NOT_FOUND") || detail.contains("Cannot find module"),
                    "expected module resolution failure in detail, got: {detail}"
                );
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn skipped_for_non_node_runtime() {
        // A provider whose program is not a node runtime does not implement the
        // --provider-contract dry-load contract and must be skipped, not failed.
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.binary.provider",
            "backend": "test",
            "invocation": { "argv": ["/usr/bin/some-native-executor", "--json"] },
        }))
        .expect("provider parses");

        match probe_provider_executor_resolves(&provider) {
            ProviderExecutorResolution::Skipped { reason } => {
                assert!(reason.contains("does not implement the --provider-contract"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn skipped_when_provider_has_no_command() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.empty.provider",
            "backend": "test",
        }))
        .expect("provider parses");

        match probe_provider_executor_resolves(&provider) {
            ProviderExecutorResolution::Skipped { reason } => {
                assert!(reason.contains("no resolvable command"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
