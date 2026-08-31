use super::artifact_finalization::{
    finalize_provider_file_artifacts, retain_failed_workspace_artifacts,
};
use super::outcome_normalization::{
    normalize_homeboy_local_artifact_sizes, normalize_provider_outcome_roles,
    push_unique_diagnostic,
};
use super::runner_readiness::{
    executable_file, provider_executable_env, resolve_executable_candidate,
};
use super::secrets::{provider_secret_env_plan_with_status, provider_secret_sources};
use super::*;
use crate::agent_task::ResolvedAgentTaskRuntimeTool;
use crate::agent_task_executor_evidence::link_latest_executor_evidence;
use crate::agent_task_process_containment::{
    contained_group_recovery_commands, AgentTaskProcessContainment, AgentTaskProcessSupervisor,
};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 16 * 1024;
const REDACTED_VALUE: &str = "[redacted]";
/// Floor and ceiling for how often the liveness watchdog re-samples the
/// provider's execution workspace for file activity. A CLI agent that edits
/// files without ever writing to stdout/stderr until its final JSON result is
/// still doing verifiable work, and only the workspace on disk can prove that
/// (#13626). Each sample forks `git`, so the interval is derived from the
/// configured liveness deadline (a quarter of it) rather than fixed: a short
/// deadline still gets several samples inside its own window, and a long one
/// never turns into its own load source by sampling a large repository every
/// tick of the poll loop.
const WORKSPACE_PROGRESS_CHECK_INTERVAL_FLOOR_MS: u64 = 200;
const WORKSPACE_PROGRESS_CHECK_INTERVAL_CEIL_MS: u64 = 5_000;
pub const PROVIDER_READINESS_RESULT_SCHEMA: &str =
    "homeboy/agent-task-provider-readiness-result/v1";
const PROVIDER_READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const PROVIDER_READINESS_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const PROVIDER_READINESS_IO_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

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
const IMMEDIATE_FAILURE_WINDOW: Duration = Duration::from_secs(10);
const IMMEDIATE_FAILURE_SIGNATURE_TEXT_LIMIT: usize = 4 * 1024;
const IMMEDIATE_FAILURE_ERROR_REF_LIMIT: usize = 8;

/// Bounded pipe activity retained until a provider reaches a terminal state.
/// The runner owns this rather than relying on provider-specific event formats.
#[derive(Default)]
struct ProviderOutputCapture {
    tail: Vec<u8>,
    full_output: Option<Vec<u8>>,
    total_bytes: u64,
    events: u64,
    last_activity_ms: u64,
}

impl ProviderOutputCapture {
    fn with_full_output() -> Self {
        Self {
            full_output: Some(Vec::new()),
            ..Self::default()
        }
    }

    fn record(&mut self, bytes: &[u8], elapsed_ms: u64) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        self.events = self.events.saturating_add(1);
        self.last_activity_ms = elapsed_ms;
        if let Some(full_output) = &mut self.full_output {
            full_output.extend_from_slice(bytes);
        }
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES {
            let excess = self.tail.len() - EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES;
            self.tail.drain(..excess);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.tail).trim().to_string()
    }

    fn full_text(&self) -> String {
        self.full_output
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_else(|| String::from_utf8_lossy(&self.tail))
            .trim()
            .to_string()
    }
}

/// Run the provider command with a bounded retry on transient provider/network
/// failures.
///
/// Transient failures (timeouts, connection resets, cURL error 28, 5xx,
/// temporarily-unavailable) are classified as retryable and retried with
/// escalating backoff. Permanent failures (auth, validation, malformed input,
/// capability gaps) fail fast on the first attempt. Each retry is surfaced in
/// the returned outcome diagnostics so the behaviour is visible in run output.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn run_materialized_provider_command(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    execution: &AgentTaskExecutionContext,
) -> AgentTaskOutcome {
    run_materialized_provider_command_with_credentials(request, provider, execution, None)
}

pub(super) fn run_materialized_provider_command_with_credentials(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    execution: &AgentTaskExecutionContext,
    credential_env: Option<&[(String, String)]>,
) -> AgentTaskOutcome {
    let mut retry_attempt = 1;
    // This state belongs to one invocation's retry sequence. It cannot couple
    // unrelated tasks that happen to use the same provider concurrently.
    let mut prior_immediate_failure = None;
    loop {
        let started = Instant::now();
        let mut outcome = run_materialized_provider_command_once_with_credentials(
            request,
            provider,
            execution,
            credential_env,
        );
        classify_provider_policy_denial(request, &mut outcome);
        classify_transient_provider_outcome(&mut outcome);

        if let Some(failure) = immediate_provider_failure(provider, &outcome, started.elapsed()) {
            if prior_immediate_failure.as_deref() == Some(failure.signature.as_str()) {
                if outcome.failure_classification != Some(AgentTaskFailureClassification::Transient)
                {
                    outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
                }
                outcome.diagnostics.push(AgentTaskDiagnostic {
                    class: "agent_task.provider_immediate_failure_retry_suppressed".to_string(),
                    message: format!(
                        "provider '{}' repeated immediate '{}' failure; same-provider retry suppressed",
                        provider.id, failure.pattern_id
                    ),
                    data: json!({
                        "provider_id": provider.id,
                        "backend": provider.backend,
                        "failure_pattern": failure.pattern_id,
                        "provider_error_refs": failure.error_refs,
                        "retryable": false,
                        "retryability_reason": "identical immediate provider failure repeated in this task/provider retry sequence",
                        "log_lookup": failure.log_lookup,
                        "fallback_action": failure.fallback_action,
                        "scope": "task_provider_retry_sequence",
                    }),
                });
                attach_runtime_tool_provenance(request, &mut outcome);
                link_latest_executor_evidence(request, &mut outcome, execution.run_id.as_deref());
                return outcome;
            }
            prior_immediate_failure = Some(failure.signature);
            // The adapter declared this server-side failure retryable. Retain
            // Homeboy's normal one-retry recovery path until the same signature
            // proves it is not a transient blip.
            outcome.failure_classification = Some(AgentTaskFailureClassification::Transient);
        }

        let retryable = outcome_is_transient(&outcome);
        if !retryable || retry_attempt >= PROVIDER_TRANSIENT_MAX_ATTEMPTS {
            if retry_attempt > 1 {
                annotate_transient_retry(&mut outcome, retry_attempt, retryable);
            }
            attach_runtime_tool_provenance(request, &mut outcome);
            // Preserve and link the latest raw executor input/result as
            // first-class run evidence before returning the final outcome.
            link_latest_executor_evidence(request, &mut outcome, execution.run_id.as_deref());
            return outcome;
        }

        let backoff_ms =
            PROVIDER_TRANSIENT_BASE_BACKOFF_MS.saturating_mul(1u64 << (retry_attempt - 1));
        if backoff_ms > 0 {
            let remaining = crate::agent_task_timeout::remaining_execution_deadline_ms(
                request.limits.execution_deadline_unix_ms,
            );
            if remaining == Some(0) {
                return execution_deadline_outcome(
                    request,
                    provider,
                    &render_provider_command_display(provider),
                    "provider_retry_backoff",
                );
            }
            std::thread::sleep(Duration::from_millis(
                remaining.map_or(backoff_ms, |remaining| remaining.min(backoff_ms)),
            ));
            if crate::agent_task_timeout::remaining_execution_deadline_ms(
                request.limits.execution_deadline_unix_ms,
            ) == Some(0)
            {
                return execution_deadline_outcome(
                    request,
                    provider,
                    &render_provider_command_display(provider),
                    "provider_retry_backoff",
                );
            }
        }
        retry_attempt += 1;
    }
}

pub(super) struct ImmediateProviderFailure {
    pattern_id: String,
    signature: String,
    pub(super) error_refs: Vec<String>,
    pub(super) log_lookup: String,
    fallback_action: String,
}

pub(super) fn immediate_provider_failure(
    provider: &AgentTaskExecutorProvider,
    outcome: &AgentTaskOutcome,
    elapsed: Duration,
) -> Option<ImmediateProviderFailure> {
    if elapsed >= IMMEDIATE_FAILURE_WINDOW
        || !matches!(
            outcome.status,
            AgentTaskOutcomeStatus::ProviderError | AgentTaskOutcomeStatus::Failed
        )
        || !matches!(
            outcome.failure_classification,
            None | Some(AgentTaskFailureClassification::Provider)
                | Some(AgentTaskFailureClassification::Transient)
        )
    {
        return None;
    }
    let text = provider_failure_text(outcome);
    provider
        .immediate_failure_patterns
        .iter()
        .find_map(|pattern| {
            if !pattern.retryable || pattern.error_contains_any.is_empty() {
                return None;
            }
            let matches = pattern.error_contains_any.iter().any(|needle| {
                !needle.is_empty()
                    && text
                        .to_ascii_lowercase()
                        .contains(&needle.to_ascii_lowercase())
            });
            matches.then(|| {
                let error_refs = pattern
                    .error_ref_pattern
                    .as_deref()
                    .and_then(|expression| regex::Regex::new(expression).ok())
                    .map(|expression| {
                        expression
                            .find_iter(&text)
                            .take(IMMEDIATE_FAILURE_ERROR_REF_LIMIT)
                            .map(|matched| matched.as_str().to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let normalized = pattern
                    .error_ref_pattern
                    .as_deref()
                    .and_then(|expression| regex::Regex::new(expression).ok())
                    .map(|expression| {
                        expression
                            .replace_all(&text, "[provider-error-ref]")
                            .into_owned()
                    })
                    .unwrap_or_else(|| text.clone());
                let normalized = bounded_text(&normalized, IMMEDIATE_FAILURE_SIGNATURE_TEXT_LIMIT);
                ImmediateProviderFailure {
                pattern_id: pattern.id.clone(),
                signature: format!(
                    "{}:{}:{}",
                    provider.id,
                    pattern.id,
                    homeboy_engine_primitives::content_hash::sha256_hex(normalized.as_bytes())
                ),
                error_refs,
                log_lookup: pattern.log_lookup.clone().unwrap_or_else(|| {
                    "homeboy agent-task logs <run-id> --task <task-id>".to_string()
                }),
                fallback_action: pattern.fallback_action.clone().unwrap_or_else(|| {
                    "Select another configured provider or pause until this provider is healthy."
                        .to_string()
                }),
            }
            })
        })
}

/// Validate adapter-owned failure signatures before they can affect dispatch.
/// Invalid regular expressions must be a visible manifest contract error, not
/// a silent opt-out from retry suppression.
pub fn validate_provider_immediate_failure_patterns(
    provider: &AgentTaskExecutorProvider,
) -> std::result::Result<(), String> {
    for pattern in &provider.immediate_failure_patterns {
        if pattern.id.trim().is_empty()
            || pattern
                .error_contains_any
                .iter()
                .all(|value| value.trim().is_empty())
        {
            return Err(
                "immediate failure patterns need an id and at least one non-empty error substring"
                    .to_string(),
            );
        }
        if let Some(expression) = &pattern.error_ref_pattern {
            if expression.len() > 512 {
                return Err(format!(
                    "immediate failure pattern '{}' error_ref_pattern exceeds 512 bytes",
                    pattern.id
                ));
            }
            let expression = regex::Regex::new(expression).map_err(|error| {
                format!(
                    "immediate failure pattern '{}' has invalid error_ref_pattern: {error}",
                    pattern.id
                )
            })?;
            if expression.is_match("") {
                return Err(format!(
                    "immediate failure pattern '{}' error_ref_pattern must not match an empty string",
                    pattern.id
                ));
            }
        }
    }
    Ok(())
}

fn provider_failure_text(outcome: &AgentTaskOutcome) -> String {
    let mut text = outcome.summary.clone().unwrap_or_default();
    for diagnostic in &outcome.diagnostics {
        text.push('\n');
        text.push_str(&diagnostic.message);
        text.push('\n');
        text.push_str(&diagnostic.data.to_string());
    }
    text
}

fn bounded_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn attach_runtime_tool_provenance(
    request: &AgentTaskExecutorRequest,
    outcome: &mut AgentTaskOutcome,
) {
    if request.resolved_runtime_tools.is_empty() {
        return;
    }
    if outcome.metadata.is_null() {
        outcome.metadata = json!({});
    }
    if let Some(metadata) = outcome.metadata.as_object_mut() {
        // This is resolved host evidence only: tool environment values never enter it.
        metadata.insert(
            "resolved_runtime_tools".to_string(),
            serde_json::to_value(
                request
                    .resolved_runtime_tools
                    .clone()
                    .into_iter()
                    .map(ResolvedAgentTaskRuntimeTool::redacted)
                    .collect::<Vec<_>>(),
            )
            .expect("resolved runtime tool provenance serializes"),
        );
        if let Some(evidence) = request.request.metadata.get("capability_evidence") {
            metadata.insert("capability_evidence".to_string(), evidence.clone());
        }
    }
}

pub(super) fn describe_controller_owned_publication(
    request: &AgentTaskExecutorRequest,
    outcome: &mut AgentTaskOutcome,
) {
    if !request.request.publication_is_controller_owned() {
        return;
    }
    if outcome.metadata.is_null() {
        outcome.metadata = json!({});
    }
    if let Some(metadata) = outcome.metadata.as_object_mut() {
        metadata.insert(
            "publication".to_string(),
            json!({
                "owner": "controller",
                "status": "not_attempted"
            }),
        );
    }
}

/// True when an outcome represents a transient provider/network failure that is
/// safe to retry.
fn outcome_is_transient(outcome: &AgentTaskOutcome) -> bool {
    outcome.failure_classification == Some(AgentTaskFailureClassification::Transient)
}

/// A provider policy rejecting a controller-projected evidence path is an
/// execution-environment contract failure, not a task failure. Mark it
/// explicitly so scheduler retry policy terminates without spending retries.
fn classify_provider_policy_denial(
    request: &AgentTaskExecutorRequest,
    outcome: &mut AgentTaskOutcome,
) {
    let Some(declared) = request.request.executor.config["evidence_inputs"].as_array() else {
        return;
    };
    let declared_paths = declared
        .iter()
        .filter_map(|input| input.get("path").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let denied_path = outcome.diagnostics.iter().find_map(|diagnostic| {
        (diagnostic.data["kind"].as_str() == Some("permission_denied"))
            .then(|| diagnostic.data["path"].as_str())
            .flatten()
    });
    if denied_path.is_some_and(|path| declared_paths.contains(path)) {
        outcome.failure_classification = Some(AgentTaskFailureClassification::PolicyDenied);
        if outcome.metadata.is_null() {
            outcome.metadata = json!({});
        }
        outcome.metadata["control_plane_failure"] = json!({
            "phase": "provider_evidence_preflight",
            "reason": "declared_evidence_policy_denied",
            "path": denied_path,
        });
    }
}

/// Promote a `ProviderError`/`Provider` outcome to the `Transient`
/// classification when its surfaced text looks like a transient network or
/// provider blip. Leaves permanent provider failures untouched so they keep
/// failing fast.
fn classify_transient_provider_outcome(outcome: &mut AgentTaskOutcome) {
    if outcome_text_is_rate_limited(outcome) {
        outcome.failure_classification = Some(AgentTaskFailureClassification::RateLimited);
        annotate_usage_cap(outcome);
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

/// Attach a [`super::usage_cap::AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS`]
/// diagnostic naming the reset time when the outcome text carries a usage-cap
/// signature the scheduler's rotation-skip logic can act on (#13644). A
/// generic rate-limit failure without a recognizable usage-cap reset stays a
/// plain `RateLimited` classification, unchanged.
fn annotate_usage_cap(outcome: &mut AgentTaskOutcome) {
    let now = chrono::Utc::now();
    let reset_at = outcome
        .summary
        .as_deref()
        .and_then(|text| super::usage_cap::detect_usage_cap(text, now))
        .or_else(|| {
            outcome.diagnostics.iter().find_map(|diagnostic| {
                super::usage_cap::detect_usage_cap(&diagnostic.message, now).or_else(|| {
                    super::usage_cap::detect_usage_cap(&diagnostic.data.to_string(), now)
                })
            })
        });
    let Some(reset_at) = reset_at else {
        return;
    };
    push_unique_diagnostic(
        &mut outcome.diagnostics,
        super::usage_cap::AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS.to_string(),
        format!(
            "provider usage cap reached; resets at {}",
            reset_at.to_rfc3339()
        ),
        json!({ "reset_at": reset_at.to_rfc3339() }),
    );
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
        "usage limit",
        "usage cap",
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

/// What Homeboy observed while tearing down the provider's contained process
/// group. A failure here is not a provider defect, so it is surfaced as an
/// additive diagnostic on whatever outcome the attempt produced rather than
/// overwriting it (#11477).
#[derive(Default)]
pub(super) struct ProviderContainmentReport {
    leader_pid: Option<u32>,
    cleanup_errors: Vec<String>,
}

impl ProviderContainmentReport {
    fn record(&mut self, supervisor: &AgentTaskProcessSupervisor, result: Result<(), String>) {
        self.leader_pid = supervisor.leader_pid();
        if let Err(error) = result {
            self.cleanup_errors.push(error);
        }
    }

    /// A terminal run that still owns live processes must say so. Nothing else
    /// in the run record distinguishes "the provider tree is gone" from "three
    /// compiler processes are still burning the host".
    fn annotate(&self, outcome: &mut AgentTaskOutcome) {
        if self.cleanup_errors.is_empty() {
            return;
        }
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "agent_task.provider_process_group_survivors".to_string(),
            format!(
                "provider process group{} could not be confirmed terminated; it may still own live processes on this host",
                self.leader_pid
                    .map(|pid| format!(" (leader pid {pid})"))
                    .unwrap_or_default()
            ),
            json!({
                "leader_pid": self.leader_pid,
                "cleanup_errors": self.cleanup_errors,
                "recovery_commands": contained_group_recovery_commands(self.leader_pid),
                "remediation_hints": [
                    "A contained provider tree that outlives its run keeps consuming host RAM, CPU, and disk. Inspect and reap it with the recovery commands before starting another run."
                ],
            }),
        );
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn run_materialized_provider_command_once(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    execution: &AgentTaskExecutionContext,
) -> AgentTaskOutcome {
    run_materialized_provider_command_once_with_credentials(request, provider, execution, None)
}

fn run_materialized_provider_command_once_with_credentials(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    execution: &AgentTaskExecutionContext,
    credential_env: Option<&[(String, String)]>,
) -> AgentTaskOutcome {
    let mut containment_report = ProviderContainmentReport::default();
    let mut outcome = run_materialized_provider_command_once_contained(
        request,
        provider,
        &mut containment_report,
        execution,
        credential_env,
    );
    containment_report.annotate(&mut outcome);
    if let Err(error) = retain_failed_workspace_artifacts(&mut outcome, request) {
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "agent_task.failed_workspace_artifact_retention_failed".to_string(),
            "Homeboy could not retain declared artifacts from the failed attempt workspace"
                .to_string(),
            json!({ "details": error.message }),
        );
    }
    outcome
}

fn run_materialized_provider_command_once_contained(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    containment_report: &mut ProviderContainmentReport,
    execution: &AgentTaskExecutionContext,
    credential_env: Option<&[(String, String)]>,
) -> AgentTaskOutcome {
    let run_id = execution.run_id.as_deref();
    let attempt = execution.attempt;
    let command = render_provider_command_display(provider);
    let deadline_remaining_ms = crate::agent_task_timeout::remaining_execution_deadline_ms(
        request.limits.execution_deadline_unix_ms,
    );
    if deadline_remaining_ms == Some(0) {
        return execution_deadline_outcome(request, provider, &command, "provider_execution");
    }
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

    // Workspace identity stays at the Git root for scheduler admission and
    // baseline replacement. A component-relative cwd is resolved only after
    // that identity is established, so nested packages execute correctly in
    // both the original worktree and isolated retry baselines.
    let attestation_root = request.request.workspace.root.as_deref().map(PathBuf::from);
    let cwd = match attestation_root.as_deref() {
        Some(root) => match homeboy_core::resolve_contained_local_path(
            root,
            request
                .request
                .executor
                .config
                .get("component_cwd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("."),
            "component_cwd",
        ) {
            Ok(cwd) => Some(cwd),
            Err(error) => {
                return failure_outcome(
                    request,
                    AgentTaskOutcomeStatus::ProviderError,
                    AgentTaskFailureClassification::InvalidInput,
                    "agent_task.component_cwd_invalid",
                    error.to_string(),
                    json!({ "provider": provider.id }),
                )
            }
        },
        None => provider_cwd,
    };
    let attestation_identity = attestation_root
        .as_deref()
        .or(cwd.as_deref())
        .map(workspace_identity)
        .transpose();
    let attestation_identity = match attestation_identity {
        Ok(identity) => identity,
        Err(message) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.workspace_identity_invalid",
                message,
                json!({ "provider": provider.id }),
            )
        }
    };

    if let Some(preflight) = provider_preflight_failure(request, provider, &program, &cwd, &command)
    {
        return preflight;
    }

    let attempt_timeout_ms = crate::agent_task_timeout::effective_provider_timeout_ms(
        request.limits.timeout_ms,
        request.limits.max_runtime_ms,
    );
    let requested_timeout_ms = deadline_remaining_ms
        .map(|remaining| attempt_timeout_ms.min(remaining))
        .unwrap_or(attempt_timeout_ms);
    // Grace is for a per-attempt timeout only. An absolute execution deadline
    // must not be extended by a process-local cleanup allowance.
    let process_timeout = if deadline_remaining_ms.is_some() {
        Duration::from_millis(requested_timeout_ms)
    } else {
        timeout_with_grace(requested_timeout_ms)
    };
    let mut provider_request = request.clone();
    provider_request.request.limits.timeout_ms = Some(requested_timeout_ms);
    provider_request.request.normalize_artifact_declarations();
    if let Err(error) = project_output_declarations_for_provider(&mut provider_request.request) {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::InvalidInput,
            "agent_task.output_declaration_invalid",
            error,
            json!({ "provider": provider.id }),
        );
    }
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
    let mut env = match provider_command_env_with_credentials(request, provider, credential_env) {
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
    // The lease remains alive through the provider process. This lets provider
    // attempts and controller gates share only the same compatibility-keyed
    // Cargo output directory while preserving their isolated HOME/XDG roots.
    let cargo_target = match provider_cargo_target(request, cwd.as_deref(), &env) {
        Ok(target) => target,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.cargo_cache_unavailable",
                error.to_string(),
                json!({ "provider": provider.id }),
            )
        }
    };
    if let Some(target) = &cargo_target {
        env.push((
            "CARGO_TARGET_DIR".to_string(),
            target.target_dir().display().to_string(),
        ));
        env.push((
            "HOMEBOY_CARGO_TARGET_RESOLUTION".to_string(),
            target.resolution().to_string(),
        ));
    }

    let launch_context = match AgentTaskProviderLaunchContext::materialize(
        &request.request,
        provider,
        execution,
        cwd.as_deref(),
        &env,
    ) {
        Ok(context) => context,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.provider_launch_context_invalid",
                error.to_string(),
                json!({ "provider": provider.id }),
            )
        }
    };
    if let (Some(store), Some(run_id)) = (execution.lifecycle_store(), run_id) {
        if let Err(error) = store.record_provider_launch_context(
            run_id,
            &request.request.task_id,
            attempt,
            &launch_context,
        ) {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_launch_context_persistence_failed",
                error.to_string(),
                json!({ "provider": provider.id, "run_id": run_id }),
            );
        }
    }

    let mut command_builder = Command::new(&program);
    if let Err(error) = launch_context.apply_declared_environment(&mut command_builder) {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::InvalidInput,
            "agent_task.provider_launch_environment_invalid",
            error.to_string(),
            json!({ "provider": provider.id }),
        );
    }
    command_builder.args(&args).envs(
        env.iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    if let Some(cwd) = cwd {
        // Old persisted Cook plans bind the source workspace. Newly materialized
        // plans replace that with an attempt-specific binding before execution.
        if let Some(attestation) = request
            .request
            .metadata
            .get("cook_attempt_workspace_identity")
            .or_else(|| request.request.metadata.get("cook_workspace_identity"))
        {
            let identity_match = crate::agent_task_workspace_identity::workspace_attestation_match(
                attestation_root.as_deref().unwrap_or(&cwd),
                attestation,
            );
            if identity_match
                != crate::agent_task_workspace_identity::WorkspaceAttestationMatch::Matched
            {
                let representation_drift = identity_match
                    == crate::agent_task_workspace_identity::WorkspaceAttestationMatch::GitRepresentationDrift;
                return failure_outcome(
                    request,
                    AgentTaskOutcomeStatus::ProviderError,
                    AgentTaskFailureClassification::InvalidInput,
                    if representation_drift {
                        "agent_task.workspace_git_representation_changed"
                    } else {
                        "agent_task.workspace_identity_changed"
                    },
                    if representation_drift {
                        "provider workspace .git representation no longer matches its Cook attempt identity attestation; refusing execution".to_string()
                    } else {
                        "provider workspace no longer matches its Cook attempt identity attestation; refusing execution".to_string()
                    },
                    json!({ "provider": provider.id, "workspace": attestation_root.as_deref().unwrap_or(&cwd), "execution_cwd": cwd }),
                );
            }
        }
        if attestation_root
            .as_deref()
            .or(Some(&cwd))
            .and_then(|path| workspace_identity(path).ok())
            .as_ref()
            != attestation_identity.as_ref()
        {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.workspace_identity_changed",
                "provider workspace changed after validation; refusing execution".to_string(),
                json!({ "provider": provider.id, "workspace": attestation_root.as_deref().unwrap_or(&cwd), "execution_cwd": cwd }),
            );
        }
        command_builder.current_dir(cwd);
    }

    command_builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // A provider agent spawns its own build/test tooling. Contain the whole
    // subtree before it exists so every terminalization path — Homeboy's
    // timeout and liveness kills below, and the death of this controller
    // process itself — reaps the descendants instead of only the direct child
    // (#11477). Fail closed: an uncontainable provider is one whose orphans
    // nothing can reap.
    let containment = match AgentTaskProcessContainment::prepare(&mut command_builder) {
        Ok(containment) => containment,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_containment_failed",
                format!(
                "Homeboy could not establish process-tree containment for provider '{}': {error}",
                provider.id
            ),
                json!({ "provider": provider.id, "command": command, "phase": "prepare" }),
            )
        }
    };

    let child = match command_builder.spawn() {
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

    let mut child = match containment.supervise(child) {
        Ok(child) => child,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_containment_failed",
                format!(
                    "Homeboy could not guard the process tree of provider '{}': {error}",
                    provider.id
                ),
                json!({ "provider": provider.id, "command": command, "phase": "attach" }),
            );
        }
    };
    if let Some(run_id) = run_id {
        // Failure to record this diagnostic identity must not interrupt provider
        // execution. The reservation remains the execution authority.
        if let Some(store) = execution.lifecycle_store() {
            let _ = crate::agent_task_lifecycle::record_provider_execution_process_in_store(
                store,
                run_id,
                &request.request.task_id,
                attempt,
                child.id(),
            );
        } else {
            let _ = crate::agent_task_lifecycle::record_provider_execution_process(
                run_id,
                &request.request.task_id,
                attempt,
                child.id(),
            );
        }
    }

    let started = Instant::now();
    // Provider stdout may be a large valid JSON outcome, so retain it for
    // parsing while the timeout diagnostic persists only its bounded tail.
    let stdout_capture = Arc::new(Mutex::new(ProviderOutputCapture::with_full_output()));
    let stderr_capture = Arc::new(Mutex::new(ProviderOutputCapture::default()));
    // Progress and the wall timeout must share a monotonic clock. A system-clock
    // adjustment must never turn a stale provider into a live one.
    let last_progress_ms: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let mut runtime_progress = runtime_progress_snapshot(&request.artifacts_path);
    let mut runtime_progress_events = 0_u64;
    // Only sampled when the workspace root is known; a provider running
    // without a declared workspace (e.g. a preflight or repo-less task) keeps
    // relying on process output and artifacts-path progress alone.
    let mut workspace_progress = attestation_root
        .as_deref()
        .map(workspace_progress_snapshot)
        .unwrap_or_default();
    let mut workspace_progress_events = 0_u64;
    let mut next_workspace_progress_check_ms = 0_u64;

    let (stdout_runtime_capture, stderr_runtime_capture) =
        runtime_output_captures(request, run_id, attempt);
    if let Some(run_id) = run_id {
        // The files exist before any output arrives, so a controller-side
        // cancellation retains resolvable diagnostics even if it interrupts us.
        let _ = crate::agent_task_lifecycle::record_provider_execution_runtime_evidence(
            run_id,
            &request.request.task_id,
            attempt,
            stdout_runtime_capture
                .as_ref()
                .map(|capture| capture.uri.clone()),
            stderr_runtime_capture
                .as_ref()
                .map(|capture| capture.uri.clone()),
        );
    }
    let stdout_reader = child.stdout.take().map(|stdout| {
        spawn_provider_output_reader(
            stdout,
            Arc::clone(&stdout_capture),
            Arc::clone(&last_progress_ms),
            started,
            stdout_runtime_capture.map(|capture| capture.file),
        )
    });
    let stderr_reader = child.stderr.take().map(|stderr| {
        spawn_provider_output_reader(
            stderr,
            Arc::clone(&stderr_capture),
            Arc::clone(&last_progress_ms),
            started,
            stderr_runtime_capture.map(|capture| capture.file),
        )
    });

    if let Some(mut stdin) = child.stdin.take() {
        let _ = Write::write_all(&mut stdin, &input);
    }

    let liveness_timeout_ms = crate::agent_task_timeout::effective_provider_liveness_timeout_ms(
        request.limits.liveness_timeout_ms,
    );
    // The provider receives `requested_timeout_ms` and gets the process grace
    // specifically to serialize its timeout outcome. A liveness deadline that
    // is equal to or later than that provider deadline must not consume the
    // grace first and misclassify the attempt as stalled.
    let liveness_timeout = (liveness_timeout_ms < requested_timeout_ms)
        .then_some(Duration::from_millis(liveness_timeout_ms));
    // A quarter of the liveness deadline guarantees several workspace samples
    // land inside any single liveness window, however short, while the floor
    // and ceiling keep a very short test deadline or a very long production
    // one from turning into an excessive or pointless `git` fork cadence.
    let workspace_progress_check_interval_ms = (liveness_timeout_ms / 4).clamp(
        WORKSPACE_PROGRESS_CHECK_INTERVAL_FLOOR_MS,
        WORKSPACE_PROGRESS_CHECK_INTERVAL_CEIL_MS,
    );
    let (status, killed_for_liveness, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false, false),
            Ok(None) => {
                let elapsed = started.elapsed();
                let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                let mut progressed = false;
                let current_runtime_progress = runtime_progress_snapshot(&request.artifacts_path);
                if runtime_progress_advanced(&runtime_progress, &current_runtime_progress) {
                    progressed = true;
                    runtime_progress_events = runtime_progress_events.saturating_add(1);
                }
                runtime_progress = current_runtime_progress;
                // Liveness is the only consumer of the workspace signal, so it
                // is only worth the `git` fork when a liveness deadline is
                // actually in force and due for a fresh sample.
                if liveness_timeout.is_some() && elapsed_ms >= next_workspace_progress_check_ms {
                    next_workspace_progress_check_ms =
                        elapsed_ms.saturating_add(workspace_progress_check_interval_ms);
                    if let Some(root) = attestation_root.as_deref() {
                        let current_workspace_progress = workspace_progress_snapshot(root);
                        if workspace_progress_advanced(
                            &workspace_progress,
                            &current_workspace_progress,
                        ) {
                            progressed = true;
                            workspace_progress_events = workspace_progress_events.saturating_add(1);
                        }
                        workspace_progress = current_workspace_progress;
                    }
                }
                if progressed {
                    last_progress_ms.store(elapsed_ms, Ordering::SeqCst);
                }
                if elapsed >= process_timeout {
                    break (None, false, true);
                }
                if let Some(liveness_timeout) = liveness_timeout {
                    let progress_age = started.elapsed().saturating_sub(Duration::from_millis(
                        last_progress_ms.load(Ordering::SeqCst),
                    ));
                    if progress_age >= liveness_timeout {
                        break (None, true, false);
                    }
                    // Wake up at the earlier of process timeout and liveness deadline.
                    let remaining_liveness = liveness_timeout.saturating_sub(progress_age);
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

    // `Child::kill` signals the direct child only; the build/test descendants
    // it spawned keep running. Terminate the contained group instead, and reap
    // it in the clean-exit case too — the orphan in #11477 outlived a provider
    // that had already exited. This also has to happen before the capture
    // readers are joined: a surviving descendant holding an inherited pipe
    // means those readers never see EOF.
    // `status.is_none()` without a Homeboy-initiated kill means the wait loop
    // itself failed. The child is then still unreaped and still running, so it
    // needs the live termination path rather than a leader-exited reap.
    let containment_cleanup = if killed_for_liveness || timed_out || status.is_none() {
        child.terminate_live()
    } else {
        child.reap_after_exit()
    };
    let cancellation_acknowledged = containment_cleanup.is_ok();
    containment_report.record(&child, containment_cleanup);

    if let Some(reader) = stdout_reader {
        reader.finish(Duration::from_millis(100));
    }
    if let Some(reader) = stderr_reader {
        reader.finish(Duration::from_millis(100));
    }

    let stdout_capture = stdout_capture.lock().expect("stdout capture");
    let stderr_capture = stderr_capture.lock().expect("stderr capture");
    let stdout = stdout_capture.full_text();
    let stderr = stderr_capture.text();

    if killed_for_liveness {
        let (status, classification, message) = classify_stall_or_rate_limit(
            &stdout,
            &stderr,
            &provider.id,
            liveness_timeout
                .expect("liveness kill requires an earlier liveness deadline")
                .as_millis(),
        );
        return failure_outcome(
            request,
            status,
            classification,
            "agent_task.provider_liveness_timeout",
            message,
            json!({
                "provider": provider.id,
                "command": command,
                "deadline": "liveness",
                "timeout_ms": requested_timeout_ms,
                "process_timeout_ms": process_timeout.as_millis(),
                "liveness_timeout_ms": liveness_timeout_ms,
                "stdout_bytes": stdout_capture.total_bytes,
                "stderr_bytes": stderr_capture.total_bytes,
                "runtime_progress_events": runtime_progress_events,
                "workspace_progress_events": workspace_progress_events,
            }),
        );
    }

    if timed_out {
        if request
            .limits
            .execution_deadline_unix_ms
            .is_some_and(|deadline| crate::agent_task_timeout::now_unix_ms() >= deadline)
        {
            return execution_deadline_outcome(request, provider, &command, "provider_execution");
        }
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::Timeout,
            AgentTaskFailureClassification::Timeout,
            "agent_task.provider_timeout",
            format!(
                "provider '{}' exceeded timeout_ms={}",
                provider.id, requested_timeout_ms
            ),
            provider_timeout_diagnostic_data(
                request,
                provider,
                &command,
                run_id,
                requested_timeout_ms,
                process_timeout.as_millis(),
                liveness_timeout_ms,
                cancellation_acknowledged,
                &stdout_capture,
                &stderr_capture,
            ),
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
    // A signal-terminated executor and an executor that ran to completion
    // without emitting an outcome are different failures with different
    // causes. Collapsing an external kill (OOM killer, runner/daemon shutdown,
    // operator SIGTERM) into `provider_empty_stdout` points the operator at
    // the provider when the real cause was the kill. Homeboy's own deadline
    // and liveness kills return above, so a signal here means termination was
    // initiated outside this wait loop.
    if let Some(signal) = exit_signal(&status) {
        if serde_json::from_str::<AgentTaskOutcome>(&stdout).is_err() {
            return signal_termination_outcome(
                request,
                provider,
                &command,
                &status,
                signal,
                &stdout,
                &stderr,
                SignalTerminationContext {
                    elapsed_ms: started.elapsed().as_millis(),
                    requested_timeout_ms,
                    process_timeout_ms: process_timeout.as_millis(),
                    liveness_timeout_ms: Some(liveness_timeout_ms),
                    execution_deadline_unix_ms: request.limits.execution_deadline_unix_ms,
                },
            );
        }
    }
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
                stdout_capture.total_bytes,
                stderr_capture.total_bytes,
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
            normalize_homeboy_local_artifact_sizes(
                &mut outcome,
                &request.artifacts_path,
                &request.artifacts_path_provenance,
                request
                    .request
                    .workspace
                    .root
                    .as_deref()
                    .map(std::path::Path::new),
            );
            validate_declared_outputs(&mut outcome, request);
            describe_controller_owned_publication(request, &mut outcome);
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
                stdout_capture.total_bytes,
                stderr_capture.total_bytes,
                &provider_output_redactions(request, provider),
            ),
        ),
    }
}

fn provider_cargo_target(
    _request: &AgentTaskExecutorRequest,
    cwd: Option<&Path>,
    environment: &[(String, String)],
) -> homeboy_core::Result<Option<homeboy_core::ManagedCargoTarget>> {
    let Some(cwd) = cwd else { return Ok(None) };
    // Attempt worktrees execute from their own portable checkout state. Ambient
    // registry resolution can scan unrelated registered repositories and cannot
    // authoritatively configure this runner-local snapshot.
    let enabled = homeboy_core::component::try_discover_from_portable(cwd)?
        .is_some_and(|component| component.managed_execution.shared_cargo_target);
    if !enabled {
        return Ok(None);
    }
    let environment = environment.iter().cloned().collect();
    homeboy_core::acquire_managed_cargo_target_for_environment(
        "agent-task-cargo",
        cwd,
        None,
        &environment,
    )
    .map(Some)
}

// Provider adapters that predate the top-level declaration field consume the
// generic input representation. Project only missing names so caller-provided
// input declarations remain authoritative during the additive migration.
fn project_output_declarations_for_provider(request: &mut AgentTaskRequest) -> Result<(), String> {
    if request.output_declarations.is_empty() {
        return Ok(());
    }
    let output_declarations = request.output_declarations.clone();

    if request.inputs.is_null() {
        request.inputs = json!({});
    }
    let inputs = request.inputs.as_object_mut().ok_or_else(|| {
        "output declarations require object task inputs for provider projection".to_string()
    })?;
    let declarations = inputs
        .entry("required_outputs")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| "inputs.required_outputs must be an array".to_string())?;

    for declaration in &output_declarations {
        if declarations.iter().any(|value| {
            value.get("name").and_then(serde_json::Value::as_str) == Some(&declaration.name)
        }) {
            continue;
        }
        declarations.push(json!({
            "name": declaration.name,
            "required": declaration.required,
            "schema": declaration.schema,
            "json_schema": declaration.structural_schema,
            "max_bytes": declaration.max_bytes,
            "evidence_relationship": declaration.evidence_relationship,
        }));
    }

    Ok(())
}

/// A clean provider process is not sufficient when its declared result contract
/// is absent, invalid, oversized, or unsupported by its required evidence.
fn validate_declared_outputs(outcome: &mut AgentTaskOutcome, request: &AgentTaskRequest) {
    if !matches!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
    ) {
        return;
    }

    for declaration in &request.output_declarations {
        let Some(value) = outcome.outputs.get(&declaration.name).cloned() else {
            if declaration.required {
                declared_output_failure(
                    outcome,
                    "agent_task.output_missing",
                    format!("required output '{}' was absent", declaration.name),
                    json!({ "output": declaration.name, "required": true }),
                );
            }
            continue;
        };

        if !declaration.structural_schema.is_null() {
            if let Err(error) = validate_output_value(&value, &declaration.structural_schema) {
                declared_output_failure(
                    outcome,
                    "agent_task.output_malformed",
                    format!(
                        "output '{}' did not satisfy its declared schema",
                        declaration.name
                    ),
                    json!({ "output": declaration.name, "validation_error": error }),
                );
            }
        }

        if let Some(max_bytes) = declaration.max_bytes {
            match serde_json::to_vec(&value) {
                Ok(encoded) if encoded.len() as u64 > max_bytes => declared_output_failure(
                    outcome,
                    "agent_task.output_oversized",
                    format!(
                        "output '{}' exceeded its declared size limit",
                        declaration.name
                    ),
                    json!({ "output": declaration.name, "max_bytes": max_bytes, "actual_bytes": encoded.len() }),
                ),
                Err(error) => declared_output_failure(
                    outcome,
                    "agent_task.output_malformed",
                    format!("output '{}' could not be encoded", declaration.name),
                    json!({ "output": declaration.name, "validation_error": error.to_string() }),
                ),
                _ => {}
            }
        }

        if let Some(requirement) = &declaration.evidence_relationship {
            let evidence_present = outcome.evidence_refs.iter().any(|evidence| {
                evidence.kind == requirement.evidence.kind
                    && evidence.uri == requirement.evidence.uri
                    && evidence.label == requirement.evidence.label
            });
            if !evidence_present {
                declared_output_failure(
                    outcome,
                    "agent_task.output_evidence_missing",
                    format!(
                        "output '{}' is missing declared supporting evidence",
                        declaration.name
                    ),
                    json!({
                        "output": declaration.name,
                        "relationship": requirement.relationship,
                        "evidence": requirement.evidence,
                    }),
                );
            }
        }
    }
}

fn declared_output_failure(
    outcome: &mut AgentTaskOutcome,
    class: &str,
    message: String,
    data: serde_json::Value,
) {
    outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
    outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
    outcome.summary = Some(message.clone());
    push_unique_diagnostic(&mut outcome.diagnostics, class.to_string(), message, data);
}

fn validate_output_value(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(serde_json::Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => return Err(format!("unsupported schema type '{expected}'")),
        };
        if !matches {
            return Err(format!("expected {expected}"));
        }
    }

    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        let Some(object) = value.as_object() else {
            return Err("required properties need an object value".to_string());
        };
        for name in required {
            let Some(name) = name.as_str() else {
                return Err("required property names must be strings".to_string());
            };
            if !object.contains_key(name) {
                return Err(format!("missing required property '{name}'"));
            }
        }
    }

    if let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        let Some(object) = value.as_object() else {
            return Err("properties need an object value".to_string());
        };
        for (name, property_schema) in properties {
            if let Some(property) = object.get(name) {
                validate_output_value(property, property_schema)
                    .map_err(|error| format!("property '{name}': {error}"))?;
            }
        }
    }

    if let Some(items) = schema.get("items") {
        let Some(values) = value.as_array() else {
            return Err("items need an array value".to_string());
        };
        for (index, item) in values.iter().enumerate() {
            validate_output_value(item, items).map_err(|error| format!("item {index}: {error}"))?;
        }
    }

    Ok(())
}

#[cfg(unix)]
fn workspace_identity(path: &std::path::Path) -> std::result::Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("provider workspace must be a non-symlink directory".to_string());
    }
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn workspace_identity(path: &std::path::Path) -> std::result::Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("provider workspace must be a non-symlink directory".to_string());
    }
    Ok(())
}

fn execution_deadline_outcome(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    command: &str,
    completed_phase: &str,
) -> AgentTaskOutcome {
    failure_outcome(
        request,
        AgentTaskOutcomeStatus::Timeout,
        AgentTaskFailureClassification::Timeout,
        "agent_task.execution_deadline_exceeded",
        format!(
            "provider '{}' was not allowed to continue because the total execution deadline expired",
            provider.id
        ),
        json!({
            "provider": provider.id,
            "command": command,
            "deadline_unix_ms": request.limits.execution_deadline_unix_ms,
            "remaining_budget_ms": 0,
            "completed_phase": completed_phase,
        }),
    )
}

#[cfg(test)]
pub(super) fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    run_id: Option<&str>,
) -> AgentTaskOutcome {
    let materialized = test_executor_request(request);
    let context = test_execution_context(run_id);
    run_materialized_provider_command(&materialized, provider, &context)
}

#[cfg(test)]
pub(super) fn run_provider_command_once(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let materialized = test_executor_request(request);
    let context = test_execution_context(None);
    run_materialized_provider_command_once(&materialized, provider, &context)
}

#[cfg(test)]
fn test_execution_context(run_id: Option<&str>) -> AgentTaskExecutionContext {
    AgentTaskExecutionContext {
        plan_id: "provider-unit-test".to_string(),
        run_id: run_id.map(str::to_string),
        attempt: 1,
        cancellation: crate::agent_task_scheduler::AgentTaskCancellationToken::default(),
        lifecycle_store: None,
        provider_capacity_key: None,
    }
}

#[cfg(test)]
fn test_executor_request(request: &AgentTaskRequest) -> AgentTaskExecutorRequest {
    let artifact_store_root = tempfile::Builder::new()
        .prefix("homeboy-agent-task-provider-test-")
        .tempdir()
        .expect("test artifact store")
        .keep();
    let artifacts_path = artifact_store_root
        .join(homeboy_core::paths::sanitize_path_segment(&request.task_id))
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&artifacts_path).expect("test executor artifact root");
    AgentTaskExecutorRequest {
        request: request.clone(),
        artifacts_root_identity: crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture_with_finalized_root(&artifacts_path, artifact_store_root.join("executor-finalized")).expect("test artifact identity"),
        artifacts_path,
        artifact_store_root,
        artifacts_path_provenance: AgentTaskArtifactsPathProvenance {
            owner: "homeboy".to_string(),
            locality: "runner".to_string(),
            plan_id: "provider-unit-test".to_string(),
            run_id: None,
            task_id: request.task_id.clone(),
            attempt: 1,
        },
        resolved_runtime_tools: Vec::new(),
    }
}

struct ProviderOutputReader {
    handle: Option<std::thread::JoinHandle<()>>,
    done: std::sync::mpsc::Receiver<()>,
}

impl ProviderOutputReader {
    fn finish(mut self, timeout: Duration) {
        if self.done.recv_timeout(timeout).is_ok() {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
        // A failed process-group cleanup can leave an inherited output pipe
        // open indefinitely. Dropping the handle keeps provider deadline
        // cleanup bounded; the containment guard continues retrying the group.
    }
}

fn spawn_provider_output_reader<R>(
    mut reader: R,
    capture: Arc<Mutex<ProviderOutputCapture>>,
    last_progress_ms: Arc<AtomicU64>,
    started: Instant,
    runtime_capture: Option<std::fs::File>,
) -> ProviderOutputReader
where
    R: Read + Send + 'static,
{
    let (done_tx, done) = std::sync::mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        let mut runtime_capture = runtime_capture;
        let mut runtime_captured = 0;
        let mut chunk = [0; 4096];
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            capture
                .lock()
                .expect("provider output capture")
                .record(&chunk[..read], elapsed_ms);
            if let Some(file) = runtime_capture.as_mut() {
                let remaining =
                    EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES.saturating_sub(runtime_captured);
                let written = remaining.min(read);
                if written > 0 && file.write_all(&chunk[..written]).is_ok() {
                    runtime_captured += written;
                }
            }
            last_progress_ms.store(elapsed_ms, Ordering::SeqCst);
        }
        let _ = done_tx.send(());
    });
    ProviderOutputReader {
        handle: Some(handle),
        done,
    }
}

/// Size every file the provider owns under its artifacts directory.
///
/// `artifacts_path` is handed to the provider as its private output directory,
/// so a file growing there is the provider doing work — whatever it is named.
///
/// This deliberately does not filter on the file name. It used to require the
/// name to contain `progress`, which made liveness depend on a runtime's naming
/// convention rather than on observable activity (#13623). A runtime that
/// streams its telemetry into, say, `<task>-<backend>-runtime-stdout.log` was
/// invisible: the watchdog read zero progress while that file grew to 94KB, and
/// killed a healthy provider as stalled. Nothing else writes here, so counting
/// every file cannot manufacture liveness the provider did not earn.
fn runtime_progress_snapshot(root: &Path) -> BTreeMap<PathBuf, u64> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = path.metadata().ok()?;
            metadata.is_file().then_some((path, metadata.len()))
        })
        .collect()
}

fn runtime_progress_advanced(
    previous: &BTreeMap<PathBuf, u64>,
    current: &BTreeMap<PathBuf, u64>,
) -> bool {
    current
        .iter()
        .any(|(path, size)| *size > previous.get(path).copied().unwrap_or_default())
}

/// Liveness evidence sampled from the provider's execution workspace rather
/// than from process output. A provider that streams nothing to stdout/stderr
/// until it serializes its final outcome still leaves a trail on disk: files
/// it edits become uncommitted worktree changes, and files it commits move
/// HEAD. Either is proof of live, ongoing work (#13626).
#[derive(Default, Clone, PartialEq, Eq)]
struct WorkspaceProgressSnapshot {
    files_changed: Option<usize>,
    head_sha: Option<String>,
}

fn workspace_progress_snapshot(root: &Path) -> WorkspaceProgressSnapshot {
    WorkspaceProgressSnapshot {
        files_changed: crate::agent_task_service::worktree_files_changed(root),
        head_sha: homeboy_core::git::head_sha(root),
    }
}

/// True when two workspace samples both produced a reading and those readings
/// differ. A sample that failed to read (`None`, e.g. a transient `git`
/// failure or a non-repository root) must never be compared against a
/// present reading — that would either manufacture false progress or mask a
/// real stall depending on which side went missing.
fn workspace_progress_advanced(
    previous: &WorkspaceProgressSnapshot,
    current: &WorkspaceProgressSnapshot,
) -> bool {
    if let (Some(previous_files), Some(current_files)) =
        (previous.files_changed, current.files_changed)
    {
        if previous_files != current_files {
            return true;
        }
    }
    if let (Some(previous_head), Some(current_head)) =
        (previous.head_sha.as_deref(), current.head_sha.as_deref())
    {
        if previous_head != current_head {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn provider_timeout_diagnostic_data(
    request: &AgentTaskExecutorRequest,
    provider: &AgentTaskExecutorProvider,
    command: &str,
    run_id: Option<&str>,
    timeout_ms: u64,
    process_timeout_ms: u128,
    liveness_timeout_ms: u64,
    cancellation_acknowledged: bool,
    stdout: &ProviderOutputCapture,
    stderr: &ProviderOutputCapture,
) -> Value {
    let last_activity = match (stdout.events > 0, stderr.events > 0) {
        (false, false) => Value::Null,
        (true, false) => json!({ "kind": "stdout", "elapsed_ms": stdout.last_activity_ms }),
        (false, true) => json!({ "kind": "stderr", "elapsed_ms": stderr.last_activity_ms }),
        (true, true) if stdout.last_activity_ms >= stderr.last_activity_ms => {
            json!({ "kind": "stdout", "elapsed_ms": stdout.last_activity_ms })
        }
        (true, true) => json!({ "kind": "stderr", "elapsed_ms": stderr.last_activity_ms }),
    };
    let redactions = provider_output_redactions(request, provider);
    let command = redact_sensitive_text(command, &redactions);
    let stdout_text = stdout.text();
    let stderr_text = stderr.text();
    let stdout_tail = redact_sensitive_text(&stdout_text, &redactions);
    let stderr_tail = redact_sensitive_text(&stderr_text, &redactions);
    let log_lookup = run_id.map_or_else(
        || "homeboy agent-task logs <run-id> --task <task-id>".to_string(),
        |run_id| {
            format!(
                "homeboy agent-task logs {run_id} --task {}",
                request.task_id
            )
        },
    );

    json!({
        "provider": provider.id,
        "provider_backend": provider.backend,
        "command": command,
        "deadline": "wall_clock",
        "timeout_ms": timeout_ms,
        "process_timeout_ms": process_timeout_ms,
        "liveness_timeout_ms": liveness_timeout_ms,
        "output_event_count": stdout.events.saturating_add(stderr.events),
        "stdout_bytes": stdout.total_bytes,
        "stderr_bytes": stderr.total_bytes,
        "last_activity": last_activity,
        "stdout_tail": bounded_executor_output(&stdout_tail),
        "stderr_tail": bounded_executor_output(&stderr_tail),
        "stdout_tail_truncated": stdout.total_bytes > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES as u64,
        "stderr_tail_truncated": stderr.total_bytes > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES as u64,
        "cancellation_requested": true,
        "cancellation_acknowledged": cancellation_acknowledged,
        "provider_boundary_evidence": "executor-result",
        "log_lookup": log_lookup,
    })
}

struct RuntimeOutputCapture {
    uri: String,
    file: std::fs::File,
}

fn runtime_output_captures(
    request: &AgentTaskExecutorRequest,
    run_id: Option<&str>,
    attempt: u32,
) -> (Option<RuntimeOutputCapture>, Option<RuntimeOutputCapture>) {
    if run_id.is_none() || std::fs::create_dir_all(&request.artifacts_path).is_err() {
        return (None, None);
    }
    let capture = |name: &str| {
        let path = request.artifacts_path.join(name);
        std::fs::File::create(&path)
            .ok()
            .map(|file| RuntimeOutputCapture {
                uri: format!("file://{}", path.display()),
                file,
            })
    };
    (
        capture(&format!("provider-runtime-stdout-{attempt}.log")),
        capture(&format!("provider-runtime-stderr-{attempt}.log")),
    )
}

fn classify_stall_or_rate_limit(
    stdout: &str,
    stderr: &str,
    provider_id: &str,
    liveness_timeout_ms: u128,
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
            "provider '{provider_id}' produced no process output, structured runtime progress, or workspace file activity before liveness_timeout_ms={liveness_timeout_ms}"
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
    stdout_bytes: u64,
    stderr_bytes: u64,
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
        "stdout_bytes": stdout_bytes,
        "stdout_truncated": stdout_bytes > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES as u64,
        "stderr": bounded_executor_output(&stderr),
        "stderr_bytes": stderr_bytes,
        "stderr_truncated": stderr_bytes > EXECUTOR_OUTPUT_CAPTURE_LIMIT_BYTES as u64,
        "remediation_hints": provider_process_remediation_hints(&stdout, &stderr),
    })
}

/// Timing/budget context captured at the moment a provider process was
/// observed to have died from a signal. Recorded verbatim in the diagnostic so
/// an operator can tell "no deadline was configured" apart from "a deadline
/// expired" without re-deriving it from the aggregate.
pub(super) struct SignalTerminationContext {
    pub elapsed_ms: u128,
    pub requested_timeout_ms: u64,
    pub process_timeout_ms: u128,
    pub liveness_timeout_ms: Option<u64>,
    pub execution_deadline_unix_ms: Option<u64>,
}

/// Human-readable name for the POSIX signals a provider process realistically
/// dies from. Unknown signals fall back to their number so the diagnostic
/// never loses the raw value.
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        _ => return None,
    })
}

fn signal_label(signal: i32) -> String {
    match signal_name(signal) {
        Some(name) => format!("signal {signal} ({name})"),
        None => format!("signal {signal}"),
    }
}

/// Best-effort attribution of who ended the process. Homeboy's own deadline and
/// liveness kills never reach this path, so anything observed here was
/// initiated outside the provider wait loop. The specific signal still narrows
/// the likely source, which is the difference between "look at the provider"
/// and "look at host memory pressure".
fn signal_termination_initiator(signal: i32) -> &'static str {
    match signal {
        9 => "external_sigkill",
        15 => "external_sigterm",
        2 | 3 => "external_interrupt",
        1 => "external_hangup",
        4 | 6 | 8 | 11 => "provider_crash",
        _ => "external_signal",
    }
}

fn signal_termination_hints(signal: i32, stdout: &str, stderr: &str) -> Vec<String> {
    let mut hints = Vec::new();
    match signal {
        9 => {
            hints.push("SIGKILL is what the Linux OOM killer sends: check `dmesg -T | grep -i -e oom -e 'killed process'` and cgroup `memory.events` on the host or CI runner that ran this provider.".to_string());
            hints.push("SIGKILL cannot be trapped, so no provider-side diagnostics exist. Re-run with more memory headroom or lower provider concurrency before blaming the provider.".to_string());
        }
        15 | 2 | 3 | 1 => {
            hints.push("Termination was requested from outside this Homeboy wait loop (operator, supervisor, runner/daemon shutdown, CI job cancellation, or a parent process-group kill).".to_string());
            hints.push("Check the supervising process (systemd unit, CI job timeout, terminal that owns the process group) before treating this as a provider defect.".to_string());
        }
        4 | 6 | 8 | 11 => {
            hints.push("This signal indicates the provider process itself crashed (illegal instruction, abort, arithmetic fault, or segfault). Capture a core dump or provider-side logs.".to_string());
        }
        _ => {}
    }
    if stdout.is_empty() && stderr.is_empty() {
        hints.push("No stdout/stderr was captured before termination, which is expected for an uncatchable or immediate kill. Absence of provider output is not evidence of a provider defect here.".to_string());
    }
    hints
}

/// Classify a provider process that died from a signal without leaving a
/// parseable outcome. Deliberately reuses the existing `provider_error` status
/// and `provider` failure classification: the wire enums are consumed by
/// independently released extensions, so this fix is additive at the schema
/// level and only replaces the misleading diagnostic class/message/data.
#[allow(clippy::too_many_arguments)]
fn signal_termination_outcome(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    command: &str,
    status: &std::process::ExitStatus,
    signal: i32,
    stdout: &str,
    stderr: &str,
    context: SignalTerminationContext,
) -> AgentTaskOutcome {
    let mut data = executor_process_diagnostic_data(
        &provider.id,
        &provider.backend,
        command,
        status,
        stdout,
        stderr,
        stdout.len() as u64,
        stderr.len() as u64,
        &provider_output_redactions(request, provider),
    );
    if let Some(object) = data.as_object_mut() {
        object.insert(
            "signal_name".to_string(),
            signal_name(signal)
                .map(|name| Value::String(name.to_string()))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "termination_initiator".to_string(),
            Value::String(signal_termination_initiator(signal).to_string()),
        );
        object.insert(
            "homeboy_initiated_termination".to_string(),
            Value::Bool(false),
        );
        object.insert(
            "likely_oom_kill".to_string(),
            Value::Bool(signal == 9 && stdout.is_empty() && stderr.is_empty()),
        );
        object.insert(
            "elapsed_ms".to_string(),
            Value::from(u64::try_from(context.elapsed_ms).unwrap_or(u64::MAX)),
        );
        object.insert(
            "timeout_ms".to_string(),
            Value::from(context.requested_timeout_ms),
        );
        object.insert(
            "process_timeout_ms".to_string(),
            Value::from(u64::try_from(context.process_timeout_ms).unwrap_or(u64::MAX)),
        );
        object.insert(
            "liveness_timeout_ms".to_string(),
            context
                .liveness_timeout_ms
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        object.insert(
            "execution_deadline_unix_ms".to_string(),
            context
                .execution_deadline_unix_ms
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        object.insert(
            "signal_remediation_hints".to_string(),
            Value::from(signal_termination_hints(signal, stdout, stderr)),
        );
    }

    let stdout_note = if stdout.is_empty() {
        "no stdout was captured".to_string()
    } else {
        format!("{} bytes of unparseable stdout were captured", stdout.len())
    };

    failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Provider,
        "agent_task.provider_signal_terminated",
        format!(
            "provider '{}' was terminated by {} after {}ms before producing an outcome ({stdout_note}); termination was not initiated by Homeboy's provider deadline or liveness watchdog",
            provider.id,
            signal_label(signal),
            context.elapsed_ms
        ),
        data,
    )
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
        stdout.len() as u64,
        stderr.len() as u64,
        &redactions,
    );
    let exit_description = status
        .code()
        .map(|code| format!("status {code}"))
        .or_else(|| exit_signal(status).map(signal_label))
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

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderReadinessInvocationResult {
    pub ready: bool,
    pub classification: String,
    pub retryable: bool,
    pub remediation: String,
    pub reason: String,
    pub cache_key: String,
    pub identity: Value,
}

pub fn run_provider_readiness_invocation(
    provider: &AgentTaskExecutorProvider,
    effective_config: &Value,
) -> Result<ProviderReadinessInvocationResult, String> {
    run_provider_readiness_invocation_with_env(provider, effective_config, &[])
}

pub(crate) fn run_provider_readiness_invocation_with_env(
    provider: &AgentTaskExecutorProvider,
    effective_config: &Value,
    credential_env: &[(String, String)],
) -> Result<ProviderReadinessInvocationResult, String> {
    run_provider_readiness_invocation_with_env_and_timeout(
        provider,
        effective_config,
        credential_env,
        PROVIDER_READINESS_TIMEOUT,
    )
}

pub(super) fn run_provider_readiness_invocation_with_env_and_timeout(
    provider: &AgentTaskExecutorProvider,
    effective_config: &Value,
    credential_env: &[(String, String)],
    timeout: Duration,
) -> Result<ProviderReadinessInvocationResult, String> {
    let Some(invocation) = provider.readiness_invocation.as_ref() else {
        return Ok(ProviderReadinessInvocationResult {
            ready: true,
            classification: "ready".to_string(),
            retryable: false,
            remediation: String::new(),
            reason: String::new(),
            cache_key: String::new(),
            identity: Value::Null,
        });
    };
    run_provider_readiness_invocation_with_timeout(
        provider,
        effective_config,
        Duration::from_millis(invocation.timeout_ms),
    )
}

fn run_provider_readiness_invocation_with_timeout(
    provider: &AgentTaskExecutorProvider,
    effective_config: &Value,
    timeout: Duration,
) -> Result<ProviderReadinessInvocationResult, String> {
    let Some(invocation) = provider.readiness_invocation.as_ref() else {
        unreachable!("readiness invocation timeout requires an invocation")
    };
    let Some((program, args, cwd)) = invocation_command_parts(provider, invocation) else {
        return Err(format!(
            "provider '{}' declares an empty readiness invocation",
            provider.id
        ));
    };
    let input = serde_json::to_vec(&json!({
        "schema": "homeboy/agent-task-provider-readiness-request/v1",
        "provider_id": provider.id,
        "backend": provider.backend,
        "effective_config": effective_config,
    }))
    .map_err(|error| format!("failed to encode provider readiness request: {error}"))?;

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Runtime readiness receives no ambient credentials. Providers explicitly
    // declare the small environment surface their bounded probe needs.
    let allowlist = invocation
        .extra
        .get("env_allowlist")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
        .collect::<Vec<_>>();
    command.env_clear();
    for name in ["PATH", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            // PATH locates the declared executable and HOME resolves an
            // explicitly configured ~/ executable; neither is a credential.
            command.env(name, value);
        }
    }
    command.envs(allowlist);
    // Request-scoped resolved values override ambient allowlisted credentials.
    // They are inherited only by this contained probe and are never serialized.
    command.envs(
        credential_env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // A readiness invocation is still provider-owned code that can spawn
    // tooling of its own; contain it so a timed-out probe cannot strand a
    // subtree (#11477).
    let containment = AgentTaskProcessContainment::prepare(&mut command)
        .map_err(|error| format!("failed to contain provider readiness invocation: {error}"))?;
    let child = command
        .spawn()
        .map_err(|error| format!("failed to spawn provider readiness invocation: {error}"))?;
    let mut child = containment
        .supervise(child)
        .map_err(|error| format!("failed to guard provider readiness invocation: {error}"))?;
    let (stdin_sender, stdin_receiver) = mpsc::sync_channel(1);
    let stdin_writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin_sender.send(stdin.write_all(&input));
        })
    });
    let stdout_reader = child.stdout.take().map(spawn_readiness_output_reader);
    let stderr_reader = child.stderr.take().map(spawn_readiness_output_reader);
    let started = Instant::now();
    let mut stdin_complete = stdin_writer.is_none();
    let terminal = loop {
        if !stdin_complete {
            match stdin_receiver.try_recv() {
                Ok(Ok(())) => stdin_complete = true,
                Ok(Err(error)) => {
                    break Err(format!(
                        "failed to write provider readiness request: {error}"
                    ))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err("provider readiness stdin writer stopped unexpectedly".to_string())
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                break Err(format!(
                    "provider '{}' readiness invocation timed out after {} seconds",
                    provider.id,
                    timeout.as_secs_f64()
                ))
            }
            Err(error) => {
                break Err(format!(
                    "failed to wait for provider readiness result: {error}"
                ))
            }
        }
    };

    // Cleanup precedes capture collection: descendants may inherit these pipes.
    // Capture is received with a deadline as a second line of defence, so even
    // an OS-level cleanup failure cannot strand readiness forever.
    let cleanup = if terminal.is_ok() {
        child.reap_after_exit()
    } else {
        child.terminate_live()
    };
    let capture_timeout = || {
        timeout
            .saturating_sub(started.elapsed())
            .min(PROVIDER_READINESS_IO_DRAIN_TIMEOUT)
    };
    let stdout = receive_readiness_output(stdout_reader, capture_timeout());
    let stderr = receive_readiness_output(stderr_reader, capture_timeout());
    drop(stdin_writer);

    if let Err(error) = cleanup {
        return Err(match terminal {
            Ok(_) => format!("failed to clean up provider readiness invocation: {error}"),
            Err(terminal_error) => format!("{terminal_error}; cleanup failed: {error}"),
        });
    }
    let status = terminal?;
    let stdout = stdout?;
    let stderr = stderr?;
    if stdout.truncated || stderr.truncated {
        return Err(format!(
            "provider '{}' readiness invocation output exceeded {} bytes per stream",
            provider.id, PROVIDER_READINESS_OUTPUT_LIMIT_BYTES
        ));
    }
    if !status.success() {
        return Err(format!(
            "provider '{}' readiness invocation exited with status {}",
            provider.id,
            status.code().unwrap_or(-1)
        ));
    }
    let result_value: Value = serde_json::from_slice(&stdout.bytes).map_err(|_| {
        format!(
            "provider '{}' readiness invocation returned malformed JSON",
            provider.id
        )
    })?;
    if result_value.get("schema").and_then(Value::as_str) != Some(PROVIDER_READINESS_RESULT_SCHEMA)
    {
        return Err(format!(
            "provider '{}' readiness invocation returned an unsupported result schema",
            provider.id
        ));
    }
    let mut result: ProviderReadinessInvocationResult = serde_json::from_value(result_value)
        .map_err(|_| {
            format!(
                "provider '{}' readiness invocation returned an invalid result",
                provider.id
            )
        })?;
    redact_readiness_credentials(&mut result, credential_env);
    Ok(result)
}

struct ReadinessOutputCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_readiness_output_reader<R>(
    mut reader: R,
) -> mpsc::Receiver<std::io::Result<ReadinessOutputCapture>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        let mut chunk = [0; 8192];
        let result = loop {
            match reader.read(&mut chunk) {
                Ok(0) => break Ok(ReadinessOutputCapture { bytes, truncated }),
                Ok(read) => {
                    let available =
                        PROVIDER_READINESS_OUTPUT_LIMIT_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..read.min(available)]);
                    truncated |= read > available;
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    receiver
}

fn receive_readiness_output(
    receiver: Option<mpsc::Receiver<std::io::Result<ReadinessOutputCapture>>>,
    timeout: Duration,
) -> Result<ReadinessOutputCapture, String> {
    let Some(receiver) = receiver else {
        return Ok(ReadinessOutputCapture {
            bytes: Vec::new(),
            truncated: false,
        });
    };
    receiver
        .recv_timeout(timeout)
        .map_err(|_| "provider readiness output pipe did not close after cleanup".to_string())?
        .map_err(|error| format!("failed to read provider readiness output: {error}"))
}

fn redact_readiness_credentials(
    result: &mut ProviderReadinessInvocationResult,
    credential_env: &[(String, String)],
) {
    let credentials = credential_env
        .iter()
        .filter_map(|(_, value)| {
            (!value.is_empty()).then(|| {
                (
                    value.as_str(),
                    format!(
                        "[REDACTED:{}]",
                        homeboy_engine_primitives::content_hash::sha256_hex(value.as_bytes())
                    ),
                )
            })
        })
        .collect::<Vec<_>>();
    for (credential, _) in &credentials {
        result.classification = result.classification.replace(credential, "[REDACTED]");
        result.reason = result.reason.replace(credential, "[REDACTED]");
        result.remediation = result.remediation.replace(credential, "[REDACTED]");
    }
    for (credential, hashed) in &credentials {
        result.cache_key = result.cache_key.replace(credential, hashed);
        redact_json_credential(&mut result.identity, credential, hashed);
    }
}

fn redact_json_credential(value: &mut Value, credential: &str, replacement: &str) {
    match value {
        Value::String(text) => *text = text.replace(credential, replacement),
        Value::Array(items) => {
            for item in items {
                redact_json_credential(item, credential, replacement);
            }
        }
        Value::Object(entries) => {
            let prior = std::mem::take(entries);
            for (key, mut item) in prior {
                redact_json_credential(&mut item, credential, replacement);
                entries.insert(key.replace(credential, replacement), item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) if value.to_string() == credential => {
            *value = Value::String(replacement.to_string());
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
pub(crate) fn run_provider_readiness_invocation_with_test_timeout(
    provider: &AgentTaskExecutorProvider,
    effective_config: &Value,
    timeout: Duration,
) -> Result<ProviderReadinessInvocationResult, String> {
    run_provider_readiness_invocation_with_timeout(provider, effective_config, timeout)
}

fn render_provider_command_template(value: &str, provider: &AgentTaskExecutorProvider) -> String {
    let extension_path = provider.extension_path.as_deref().unwrap_or_default();
    let runtime_path = provider.runtime_path.as_deref().unwrap_or(extension_path);
    value
        .replace("{{extension_path}}", extension_path)
        .replace("{{runtime_path}}", runtime_path)
}

fn invocation_command_parts(
    provider: &AgentTaskExecutorProvider,
    invocation: &CommandInvocation,
) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    let mut argv = invocation
        .argv
        .iter()
        .map(|arg| render_provider_command_template(arg, provider));
    let program = argv.next()?;
    let cwd = invocation
        .cwd
        .as_deref()
        .map(|cwd| PathBuf::from(render_provider_command_template(cwd, provider)));
    Some((program, argv.collect(), cwd))
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

pub fn provider_command_parts(
    provider: &AgentTaskExecutorProvider,
) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    if !provider.invocation.argv.is_empty() {
        invocation_command_parts(provider, &provider.invocation)
    } else {
        let mut argv = render_provider_command_argv(provider).into_iter();
        let program = argv.next()?;
        Some((program, argv.collect(), None))
    }
}

/// Result of probing whether a provider's executor entrypoint actually loads on
/// disk — i.e. its module require graph resolves against the materialized
/// runtime layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderExecutorResolution {
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
pub fn probe_provider_executor_resolves(
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

    // The probe loads the provider's full require graph, which can itself
    // spawn helpers. Contain it so a timed-out probe leaves nothing behind
    // (#11477).
    let containment = match AgentTaskProcessContainment::prepare(&mut command_builder) {
        Ok(containment) => containment,
        Err(error) => {
            return ProviderExecutorResolution::Unresolved {
                command: command.clone(),
                detail: format!("failed to contain executor probe: {error}"),
            };
        }
    };

    let child = match command_builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProviderExecutorResolution::Unresolved {
                command: command.clone(),
                detail: format!("failed to spawn executor probe: {error}"),
            };
        }
    };

    let mut child = match containment.supervise(child) {
        Ok(child) => child,
        Err(error) => {
            return ProviderExecutorResolution::Unresolved {
                command: command.clone(),
                detail: format!("failed to guard executor probe: {error}"),
            };
        }
    };

    let stderr_reader = child.stderr.take().map(|mut stderr| {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = std::io::Read::by_ref(&mut stderr)
                .take(64 * 1024)
                .read_to_end(&mut buffer);
            let _ = send.send(buffer);
        });
        receive
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= EXECUTOR_RESOLUTION_PROBE_TIMEOUT {
                    let _ = child.terminate_live();
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

    // Reap before draining stderr: a surviving descendant holding the
    // inherited pipe would block `read_to_end` indefinitely.
    let _ = child.reap_after_exit();

    if status.success() {
        return ProviderExecutorResolution::Resolved;
    }

    let stderr = stderr_reader
        .and_then(|reader| reader.recv_timeout(Duration::from_millis(100)).ok())
        .map(|buffer| String::from_utf8_lossy(&buffer).trim().to_string())
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

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn provider_command_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, ProviderCommandEnvError> {
    provider_command_env_with_credentials(request, provider, None)
}

fn provider_command_env_with_credentials(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    credential_env: Option<&[(String, String)]>,
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
    if let Some(credential_env) = credential_env {
        env.extend(credential_env.iter().cloned());
        let bound_names = credential_env
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let remaining_names = secret_env_plan
            .secret_env_names()
            .into_iter()
            .filter(|name| !bound_names.contains(name.as_str()))
            .collect::<Vec<_>>();
        env.extend(
            resolve_secret_env_with_fallbacks(
                &remaining_names,
                &provider_secret_sources(provider, Some(request)),
            )
            .map_err(ProviderCommandEnvError::Secret)?,
        );
    } else {
        env.extend(
            resolve_secret_env_with_fallbacks(
                &secret_env_plan.secret_env_names(),
                &provider_secret_sources(provider, Some(request)),
            )
            .map_err(ProviderCommandEnvError::Secret)?,
        );
    }
    // Enforce the executor git-mutation boundary: strip git push credentials so a
    // provider cannot push a candidate to a real remote from its isolated attempt
    // checkout before Homeboy's verification/promotion/finalization. Homeboy
    // harvests candidates via local `git diff` and performs its own commit/push
    // from a separate finalization worktree, so the provider never needs network
    // git. Appended last so these override any inherited auth env. (#8486)
    env.extend(git_mutation_boundary_env());
    Ok(env)
}

/// Environment that denies a provider the ability to authenticate a `git push`.
///
/// Blocks every credential path: no askpass helper, no interactive terminal
/// prompt, no system config, and an empty `credential.helper` (which resets any
/// inherited helper list) so a cached-credential helper cannot supply a token.
/// A push to an authenticated remote (production `origin`) then fails regardless
/// of the remote name used, so this holds even if a provider adds a new remote.
fn git_mutation_boundary_env() -> Vec<(String, String)> {
    vec![
        ("GIT_ASKPASS".to_string(), "/bin/false".to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        // Inject `credential.helper=` via git's environment config so it resets
        // any inherited helper list (git-config(1) empty-helper semantics).
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            "credential.helper".to_string(),
        ),
        ("GIT_CONFIG_VALUE_0".to_string(), String::new()),
    ]
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
        task_id: request.task_id.clone(),
        status,
        summary: Some(message.clone()),
        failure_classification: Some(classification),
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
        ..Default::default()
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

#[cfg(all(test, unix))]
mod readiness_process_tests {
    use super::*;

    fn readiness_provider(script: &str, extra_args: &[String]) -> AgentTaskExecutorProvider {
        let mut argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        argv.extend_from_slice(extra_args);
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.readiness.provider",
            "backend": "test",
            "invocation": { "argv": ["ignored"] },
        }))
        .expect("provider parses");
        provider.readiness_invocation = Some(CommandInvocation {
            argv,
            ..CommandInvocation::default()
        });
        provider
    }

    fn wait_until_not_running(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !homeboy_core::engine::command::process_is_running(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn recorded_pid(path: &Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::read_to_string(path)
            .expect("descendant pid recorded")
            .trim()
            .parse()
            .expect("numeric descendant pid")
    }

    #[test]
    fn readiness_rejects_noisy_stdout_and_stderr_with_bounded_capture() {
        let provider = readiness_provider(
            "cat >/dev/null; dd if=/dev/zero bs=70000 count=1 2>/dev/null; dd if=/dev/zero bs=70000 count=1 1>&2 2>/dev/null",
            &[],
        );

        let error = run_provider_readiness_invocation_with_env_and_timeout(
            &provider,
            &Value::Null,
            &[],
            Duration::from_secs(5),
        )
        .expect_err("oversized readiness output is rejected");

        assert!(
            error.contains("output exceeded 65536 bytes per stream"),
            "{error}"
        );
    }

    #[test]
    fn readiness_does_not_wait_for_a_descendant_held_output_pipe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let provider = readiness_provider(
            "cat >/dev/null; sleep 30 & echo $! > \"$1\"; printf '%s' '{\"schema\":\"homeboy/agent-task-provider-readiness-result/v1\",\"ready\":true,\"classification\":\"ready\",\"retryable\":false,\"remediation\":\"\",\"reason\":\"\",\"cache_key\":\"test\",\"identity\":null}'",
            &["readiness-test".to_string(), pid_file.display().to_string()],
        );
        let started = Instant::now();

        let result = run_provider_readiness_invocation_with_env_and_timeout(
            &provider,
            &Value::Null,
            &[],
            Duration::from_secs(5),
        )
        .expect("readiness result returns after descendant cleanup");

        assert!(result.ready);
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = recorded_pid(&pid_file);
        assert!(
            wait_until_not_running(pid),
            "descendant {pid} survived cleanup"
        );
    }

    #[test]
    fn failed_readiness_stdin_terminates_and_reaps_the_process_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let provider = readiness_provider(
            "sleep 30 & echo $! > \"$1\"; exec 0<&-; sleep 1",
            &["readiness-test".to_string(), pid_file.display().to_string()],
        );
        let config = json!({ "large": "x".repeat(2 * 1024 * 1024) });

        let error = run_provider_readiness_invocation_with_env_and_timeout(
            &provider,
            &config,
            &[],
            Duration::from_secs(5),
        )
        .expect_err("closed readiness stdin fails");

        assert!(
            error.contains("failed to write provider readiness request"),
            "{error}"
        );
        let pid = recorded_pid(&pid_file);
        assert!(
            wait_until_not_running(pid),
            "descendant {pid} survived failed stdin cleanup"
        );
    }

    #[test]
    fn readiness_timeout_terminates_and_reaps_the_process_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let provider = readiness_provider(
            "cat >/dev/null; sleep 30 & echo $! > \"$1\"; wait",
            &["readiness-test".to_string(), pid_file.display().to_string()],
        );

        let error = run_provider_readiness_invocation_with_env_and_timeout(
            &provider,
            &Value::Null,
            &[],
            Duration::from_millis(100),
        )
        .expect_err("readiness invocation times out");

        assert!(error.contains("timed out"), "{error}");
        let pid = recorded_pid(&pid_file);
        assert!(
            wait_until_not_running(pid),
            "descendant {pid} survived timeout cleanup"
        );
    }
}
