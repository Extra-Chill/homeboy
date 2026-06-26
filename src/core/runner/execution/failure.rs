use serde_json::{json, Value};

use crate::core::error::{Error, ErrorCode};
use crate::core::redaction::{redact_argv, redact_argv_display};

#[allow(unused_imports)]
use super::*;

pub fn runner_exec_failure_error(output: &RunnerExecOutput) -> Option<Error> {
    if output.exit_code == 0 {
        return None;
    }

    let runner_error = find_runner_homeboy_error(output);
    let runner_code = runner_error_str_field(runner_error.as_ref(), "code");
    let runner_message = runner_error_str_field(runner_error.as_ref(), "message");
    let cause = runner_message
        .or(runner_code)
        .or_else(|| first_non_empty_line(&output.stderr))
        .or_else(|| first_non_empty_line(&output.stdout))
        .unwrap_or("runner command exited non-zero")
        .to_string();
    let execution = serde_json::to_value(output).unwrap_or(Value::Null);
    let command = redact_argv_display(&output.argv);
    let redacted_argv = redact_argv(&output.argv);
    let mut details = json!({
        "runner_id": output.runner_id,
        "job_id": output.job_id,
        "remote_cwd": output.remote_cwd,
        "command": redacted_argv,
        "exit_code": output.exit_code,
        "execution": execution,
    });
    if let Some(runner_error) = runner_error {
        details["runner_error"] = runner_error;
    }
    let failure_context = runner_exec_failure_context_from_output(output);
    if let Some(failure_context) = failure_context.as_ref() {
        details["failure_context"] = serde_json::to_value(failure_context).unwrap_or(Value::Null);
    }

    let mut error = Error::new(
        ErrorCode::RemoteCommandFailed,
        format!(
            "Runner command failed on `{}` with exit code {}: {}",
            output.runner_id, output.exit_code, cause
        ),
        details,
    )
    .with_hint(format!(
        "Runner `{}` executed `{}` from `{}`.",
        output.runner_id, command, output.remote_cwd
    ))
    .with_hint(runner_exec_failure_context_hint(output))
    .with_hint(
        "Homeboy parsed runner-side JSON errors from stdout, stderr, and job event messages when present; inspect error.details.execution for the full job evidence."
            .to_string(),
    );
    if let Some(failure_context) = failure_context.as_ref() {
        if let Some(hint) = runner_exec_failure_context_remediation_hint(failure_context) {
            error = error.with_hint(hint);
        }
    }

    Some(error)
}

/// Reads a string field out of an optional structured runner error object.
/// Both the `code` and `message` extractions in [`runner_exec_failure_error`]
/// share this identical lookup.
fn runner_error_str_field<'a>(runner_error: Option<&'a Value>, key: &str) -> Option<&'a str> {
    runner_error
        .and_then(|error| error.get(key))
        .and_then(Value::as_str)
}

pub(super) fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn runner_exec_failure_context_from_output(
    output: &RunnerExecOutput,
) -> Option<RunnerExecFailureContext> {
    runner_exec_failure_context(RunnerExecFailureContextInput {
        runner_id: &output.runner_id,
        job_id: output.job_id.as_deref(),
        persisted_run_id: output.mirror_run_id.as_deref(),
        command: &output.argv,
        exit_code: output.exit_code,
        result: None,
        stdout: &output.stdout,
        stderr: &output.stderr,
    })
}

pub(super) fn runner_exec_failure_context(
    input: RunnerExecFailureContextInput<'_>,
) -> Option<RunnerExecFailureContext> {
    if input.exit_code == 0 {
        return None;
    }

    let runner_error = input
        .result
        .and_then(find_homeboy_error_in_result_value)
        .or_else(|| find_homeboy_error_in_text(input.stdout))
        .or_else(|| find_homeboy_error_in_text(input.stderr));
    let contract_field = runner_error
        .as_ref()
        .and_then(error_contract_field)
        .map(str::to_string);
    let error_code = runner_error
        .as_ref()
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_message = runner_error
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_details = runner_error
        .as_ref()
        .and_then(|error| error.get("details"))
        .cloned();
    let reason = error_message
        .clone()
        .or_else(|| error_code.clone())
        .or_else(|| first_non_empty_line(input.stderr).map(str::to_string))
        .or_else(|| first_non_empty_line(input.stdout).map(str::to_string))
        .unwrap_or_else(|| "runner command exited non-zero".to_string());

    Some(RunnerExecFailureContext {
        schema: "homeboy/runner-exec-failure-context/v1",
        runner_id: input.runner_id.to_string(),
        job_id: input.job_id.map(str::to_string),
        persisted_run_id: input.persisted_run_id.map(str::to_string),
        command: input.command.to_vec(),
        exit_code: input.exit_code,
        contract_field,
        reason,
        error_code,
        error_message,
        error_details,
    })
}

pub(super) fn runner_exec_failure_context_hint(output: &RunnerExecOutput) -> String {
    let Some(context) = runner_exec_failure_context_from_output(output) else {
        return "Canonical failed command context was unavailable; inspect error.details.execution for raw runner evidence.".to_string();
    };
    let job = context.job_id.as_deref().unwrap_or("unknown runner job");
    let run = context
        .persisted_run_id
        .as_deref()
        .unwrap_or("unknown persisted run");
    let mut hint = format!(
        "Canonical failed command: `{}`; runner job: `{job}`; persisted run: `{run}`",
        redact_argv_display(&context.command)
    );
    append_failure_context_error_summary(&mut hint, &context);
    hint.push('.');
    hint
}

pub(crate) fn append_failure_context_error_summary(
    summary: &mut String,
    context: &RunnerExecFailureContext,
) {
    if let Some(field) = context.contract_field.as_deref() {
        summary.push_str(&format!("; contract field: `{field}`"));
    }
    if let Some(code) = context.error_code.as_deref() {
        summary.push_str(&format!("; structured error: `{code}`"));
    }
    if let Some(details) = context.error_details.as_ref() {
        summary.push_str(&format!("; details: {details}"));
    }
    summary.push_str(&format!("; reason: {}", context.reason));
}

pub(crate) fn runner_exec_failure_context_remediation_hint(
    context: &RunnerExecFailureContext,
) -> Option<String> {
    match context.error_code.as_deref() {
        Some("rig.not_found") => Some(format!(
            "Verify the rig exists on runner `{}` with `homeboy runner exec {} -- homeboy rig list`, then rerun with an existing rig or register/sync the missing rig on that runner.",
            context.runner_id, context.runner_id
        )),
        _ => None,
    }
}

pub(super) fn error_contract_field(error: &Value) -> Option<&str> {
    error
        .get("details")
        .and_then(|details| details.get("field"))
        .and_then(Value::as_str)
        .or_else(|| error.get("field").and_then(Value::as_str))
        .or_else(|| {
            error
                .get("details")
                .and_then(|details| details.get("contract_field"))
                .and_then(Value::as_str)
        })
        .or_else(|| error.get("contract_field").and_then(Value::as_str))
}

pub(super) fn find_homeboy_error_in_result_value(value: &Value) -> Option<Value> {
    homeboy_error_from_envelope(value)
        .or_else(|| {
            value
                .get("stdout")
                .and_then(Value::as_str)
                .and_then(find_homeboy_error_in_text)
        })
        .or_else(|| {
            value
                .get("stderr")
                .and_then(Value::as_str)
                .and_then(find_homeboy_error_in_text)
        })
        .or_else(|| {
            value
                .get("error")
                .filter(|error| error.is_object())
                .cloned()
        })
}

pub(super) fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

pub(super) fn find_runner_homeboy_error(output: &RunnerExecOutput) -> Option<Value> {
    find_homeboy_error_in_text(&output.stdout)
        .or_else(|| find_homeboy_error_in_text(&output.stderr))
        .or_else(|| {
            output.job_events.as_ref().and_then(|events| {
                events.iter().find_map(|event| {
                    event
                        .message
                        .as_deref()
                        .and_then(find_homeboy_error_in_text)
                        .or_else(|| event.data.as_ref().and_then(homeboy_error_from_envelope))
                })
            })
        })
}

pub(super) fn find_homeboy_error_in_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_homeboy_error_json(trimmed)
        .or_else(|| {
            trimmed
                .lines()
                .find_map(|line| parse_homeboy_error_json(line.trim()))
        })
        .or_else(|| {
            let start = trimmed.find('{')?;
            let end = trimmed.rfind('}')?;
            if end <= start {
                return None;
            }
            parse_homeboy_error_json(&trimmed[start..=end])
        })
}

pub(super) fn parse_homeboy_error_json(candidate: &str) -> Option<Value> {
    serde_json::from_str::<Value>(candidate)
        .ok()
        .and_then(|value| homeboy_error_from_envelope(&value))
}

pub(super) fn homeboy_error_from_envelope(value: &Value) -> Option<Value> {
    if value.get("success").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = value.get("error")?;
    if error.is_null() {
        return None;
    }
    if error.is_object() {
        return Some(error.clone());
    }
    Some(json!({ "message": error }))
}

/// Returns the structured failure detail for a daemon exec response, or `None`
/// when the daemon answered without any usable error/data payload (the classic
/// stale-or-restarting daemon signature behind the historical
/// `daemon exec request failed: null` symptom in #3631 / #3624).
pub(super) fn daemon_failure_payload_message(envelope: &DaemonEnvelope) -> Option<String> {
    let payload = envelope
        .error
        .as_ref()
        .or(envelope.data.as_ref())
        .filter(|value| !value.is_null())?;

    let code = payload.get("error").and_then(Value::as_str);
    let message = payload.get("message").and_then(Value::as_str);
    Some(match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => payload.to_string(),
    })
}

/// Build the controller-facing error for a daemon exec submission that came back
/// with a failure envelope. When the daemon returned no usable error payload we
/// treat it as a stale/restarting daemon and surface reconnect guidance instead
/// of the historical opaque `null` (#3631, #3624).
pub(super) fn daemon_exec_request_failed_error(
    runner_id: &str,
    status_code: u16,
    envelope: &DaemonEnvelope,
) -> Error {
    match daemon_failure_payload_message(envelope) {
        Some(detail) => Error::internal_unexpected(format!(
            "daemon exec request failed: {detail}"
        ))
        .with_hint(format!(
            "Runner `{runner_id}` daemon rejected the exec request (HTTP {status_code})."
        )),
        None => Error::internal_unexpected(format!(
            "runner `{runner_id}` daemon returned no result for the exec request (HTTP {status_code} with an empty error payload); the daemon is likely stale or was restarted"
        ))
        .with_hint(format!(
            "Reconnect the runner with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}`, then retry. If it persists, kill any stale daemon with `homeboy runner doctor {runner_id}`."
        ))
        .with_hint(
            "A daemon that reports SSH-healthy can still serve a stale process; reconnecting rebinds the tunnel to the live daemon.".to_string(),
        ),
    }
}

pub(super) fn daemon_exec_loopback_transport_error(runner_id: &str, err: std::io::Error) -> Error {
    Error::internal_unexpected(format!(
        "could not reach runner `{runner_id}` daemon to submit the exec request over loopback HTTP: {err}"
    ))
    .with_hint(format!(
        "The daemon tunnel may be stale or the daemon may have restarted. Reconnect with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}` and retry."
    ))
}

/// The exec response body could not be parsed as a daemon envelope — typically a
/// stale daemon answering with an empty or non-JSON body (#3631, #3624).
pub(super) fn daemon_exec_stale_response_error(
    runner_id: &str,
    status_code: u16,
    parse_err: &str,
) -> Error {
    Error::internal_unexpected(format!(
        "runner `{runner_id}` daemon returned an unreadable exec response (HTTP {status_code}): {parse_err}; the daemon is likely stale or was restarted mid-request"
    ))
    .with_hint(format!(
        "Reconnect with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}` and retry; if a stale daemon PID lingers, run `homeboy runner doctor {runner_id}`."
    ))
}
