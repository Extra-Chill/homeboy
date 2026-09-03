//! Provider structured runtime errors (#13703).
//!
//! When a provider runtime dies, its CLI exit code says almost nothing: the
//! actionable cause — an account-level 403 with a human-readable message and
//! an explicit non-retryable flag — is emitted as a structured error event on
//! the provider's runtime stdout stream. Diagnose used to collapse that to
//! "OpenCode CLI exited with status 1" and leave `stdout_excerpt: null` while
//! the actual error sat in a log file the same response already listed.
//!
//! This module is the provider adapter boundary for that evidence:
//! - Vendor stream shapes (currently the OpenCode JSONL error event) are
//!   parsed HERE and nowhere else.
//! - Everything downstream — diagnose projection, status summaries, the
//!   runtime evidence index — consumes only the normalized
//!   [`PROVIDER_STRUCTURED_ERROR_SCHEMA`] shape, never a vendor payload.
//!
//! The normalized error also carries a projection-level
//! `failure_classification` that separates a provider account block from a
//! code-level execution failure. Rotation policy on top of that distinction is
//! owned by #13691; this module only makes the classification observable.

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use homeboy_core::redaction::RedactionPolicy;

use crate::agent_task::AgentTaskFailureClassification;

/// Schema of the normalized structured error consumed by generic diagnose and
/// status code. Vendor payloads are always converted to this shape first.
pub const PROVIDER_STRUCTURED_ERROR_SCHEMA: &str = "homeboy/provider-structured-error/v1";

/// Byte budget hydrated from the END of a provider runtime stream. Terminal
/// error events are appended last, so a tail — not a head — carries the
/// cause; the budget keeps a runaway log out of every diagnose payload.
pub const RUNTIME_STREAM_TAIL_BYTES: usize = 8 * 1024;

/// The provider account or spending quota rejected the request. Permanent
/// until the account changes: retrying the same provider cannot succeed.
pub const PROVIDER_ACCOUNT_BLOCKED: &str = "provider_account_blocked";

/// The selected route exhausted an allocated provider usage quota.
pub const PROVIDER_QUOTA_EXHAUSTED: &str = "provider_quota_exhausted";

/// The selected route is blocked by billing, payment, credit, or subscription
/// state.
pub const PROVIDER_BILLING_BLOCKED: &str = "provider_billing_blocked";

/// The selected route's credential or authentication material was rejected.
pub const PROVIDER_CREDENTIALS_EXHAUSTED: &str = "provider_credentials_exhausted";

/// The provider throttled the request (HTTP 429 or an explicit retryable
/// flag). Retrying — possibly on another provider route — can succeed.
pub const PROVIDER_RATE_LIMITED: &str = "provider_rate_limited";

/// Any other structured provider error (bad request, upstream 5xx surfaced as
/// an error event, ...). No account-level conclusion can be drawn.
pub const PROVIDER_ERROR: &str = "provider_error";

/// Normalize a provider runtime stream into the normalized structured error.
///
/// `backend` selects the registered adapter:
/// - `Some(backend)` uses only that backend's parser (execution time, where
///   the executor backend is known), and returns `None` for unregistered
///   backends so an unrelated provider's output is never misread.
/// - `None` tries every registered adapter (read time, where only the stream
///   text is available). Every adapter is structurally strict — it requires a
///   terminal error event carrying a non-empty message — so cross-vendor
///   false positives are not realistic.
///
/// The returned value is redacted; it is safe to persist and project.
pub fn normalize_runtime_stream_error(backend: Option<&str>, text: &str) -> Option<Value> {
    let normalized = match backend {
        Some("opencode") => normalize_opencode_stream_error(text),
        Some(_) => None,
        None => normalize_opencode_stream_error(text),
    }?;
    Some(RedactionPolicy::default().redact_json(&normalized))
}

/// Recognize an already-normalized structured error so generic consumers can
/// trust the shape without knowing any vendor. Returns the value unchanged.
pub fn normalized_structured_error(value: &Value) -> Option<Value> {
    let schema_matches =
        value.get("schema").and_then(Value::as_str) == Some(PROVIDER_STRUCTURED_ERROR_SCHEMA);
    let has_message = value
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|message| !message.trim().is_empty());
    (schema_matches && has_message).then(|| value.clone())
}

/// Projection-level failure classification for a normalized structured error.
///
/// Permanent capacity rejections are divided into quota, billing, and
/// credential failures. An HTTP 403 alone can also mean a forbidden resource,
/// so classification requires explicit permanent evidence plus relevant
/// provider-neutral vocabulary.
pub fn structured_error_failure_classification(
    message: &str,
    status_code: Option<i64>,
    retryable: Option<bool>,
) -> &'static str {
    if matches!(status_code, Some(429)) || retryable == Some(true) {
        return PROVIDER_RATE_LIMITED;
    }
    if permanently_rejected(message, status_code, retryable) {
        let lowered = message.to_ascii_lowercase();
        if contains_any(
            &lowered,
            &[
                "credential",
                "api key",
                "api-key",
                "token",
                "authentication",
                "authenticate",
                "unauthorized",
                "expired key",
                "invalid key",
            ],
        ) {
            return PROVIDER_CREDENTIALS_EXHAUSTED;
        }
        if contains_any(
            &lowered,
            &[
                "billing",
                "payment",
                "credit",
                "spending limit",
                "spending-limit",
                "subscription",
                "run out of",
            ],
        ) {
            return PROVIDER_BILLING_BLOCKED;
        }
        if contains_any(
            &lowered,
            &["quota", "usage limit", "usage cap", "allowance"],
        ) {
            return PROVIDER_QUOTA_EXHAUSTED;
        }
    }
    PROVIDER_ERROR
}

fn permanently_rejected(message: &str, status_code: Option<i64>, retryable: Option<bool>) -> bool {
    let lowered = message.to_ascii_lowercase();
    let vocabulary_hit = contains_any(
        &lowered,
        &[
            "credit",
            "quota",
            "spending limit",
            "spending-limit",
            "subscription",
            "billing",
            "payment",
            "insufficient",
            "run out of",
            "credential",
            "api key",
            "api-key",
            "token",
            "authentication",
            "authenticate",
            "unauthorized",
            "expired key",
            "invalid key",
            "usage limit",
            "usage cap",
            "allowance",
        ],
    );
    let account_status = matches!(status_code, Some(401 | 402 | 403));
    match retryable {
        // The provider itself declared the failure permanent; billing
        // vocabulary is then sufficient evidence of an account-level cause.
        Some(false) => vocabulary_hit,
        // Without an explicit flag, require both signals so a plain 403 on a
        // forbidden resource stays a generic provider error.
        None => account_status && vocabulary_hit,
        Some(true) => false,
    }
}

fn contains_any(message: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| message.contains(pattern))
}

/// Build the normalized error from parsed fields.
fn normalized_error(
    message: &str,
    status_code: Option<i64>,
    retryable: Option<bool>,
    error_name: Option<&str>,
) -> Value {
    json!({
        "schema": PROVIDER_STRUCTURED_ERROR_SCHEMA,
        "message": message,
        "status_code": status_code,
        "retryable": retryable,
        "failure_classification": structured_error_failure_classification(
            message,
            status_code,
            retryable,
        ),
        "error_name": error_name,
    })
}

/// Parse the OpenCode runtime stream for its terminal error event.
///
/// OpenCode emits JSONL events on runtime stdout; a fatal provider/API error
/// is `{"type":"error","error":{"name":"APIError","data":{"message":...,
/// "statusCode":403,"isRetryable":false}}}` (observed live in #13703). The
/// whole stream may also be a single JSON object. The LAST error event wins:
/// it is the terminal cause.
fn normalize_opencode_stream_error(text: &str) -> Option<Value> {
    let mut last_error = None;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(error) = structured_error_from_opencode_event(&event) {
            last_error = Some(error);
            break;
        }
    }
    if last_error.is_none() {
        if let Ok(event) = serde_json::from_str::<Value>(text) {
            last_error = structured_error_from_opencode_event(&event);
        }
    }
    last_error
}

fn structured_error_from_opencode_event(event: &Value) -> Option<Value> {
    if event.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let error = event.get("error")?;
    // The observed shape nests the payload under `data`; accept the error
    // object itself as a fallback for revisions that flatten it.
    let payload = error.get("data").unwrap_or(error);
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())?;
    let status_code = payload
        .get("statusCode")
        .or_else(|| payload.get("status_code"))
        .and_then(Value::as_i64);
    let retryable = payload
        .get("isRetryable")
        .or_else(|| payload.get("retryable"))
        .and_then(Value::as_bool);
    Some(normalized_error(
        message,
        status_code,
        retryable,
        error.get("name").and_then(Value::as_str),
    ))
}

/// Normalize a provider error already captured in a provider result. This is
/// the same adapter path used for runtime streams, so generic outcome code
/// never inspects OpenCode's vendor payload.
pub fn normalize_provider_error(backend: &str, value: &Value) -> Option<Value> {
    let normalized = match backend {
        "opencode" => normalize_opencode_error_value(value),
        _ => None,
    }?;
    Some(RedactionPolicy::default().redact_json(&normalized))
}

fn normalize_opencode_error_value(value: &Value) -> Option<Value> {
    if let Some(error) = structured_error_from_opencode_event(value) {
        return Some(error);
    }
    match value {
        Value::Object(object) => {
            let payload = object.get("data").unwrap_or(value);
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty());
            if let Some(message) = message {
                let status_code = payload
                    .get("statusCode")
                    .or_else(|| payload.get("status_code"))
                    .and_then(Value::as_i64);
                let retryable = payload
                    .get("isRetryable")
                    .or_else(|| payload.get("is_retryable"))
                    .or_else(|| payload.get("retryable"))
                    .and_then(Value::as_bool);
                return Some(normalized_error(
                    message,
                    status_code,
                    retryable,
                    object.get("name").and_then(Value::as_str),
                ));
            }
            object.values().find_map(normalize_opencode_error_value)
        }
        Value::Array(values) => values.iter().find_map(normalize_opencode_error_value),
        _ => None,
    }
}

/// Convert a normalized provider error into the generic failure type used by
/// scheduling. Unknown adapter failures remain ordinary execution failures.
pub fn normalized_error_failure_classification(
    value: &Value,
) -> Option<AgentTaskFailureClassification> {
    match value.get("failure_classification").and_then(Value::as_str) {
        Some(PROVIDER_QUOTA_EXHAUSTED) => {
            Some(AgentTaskFailureClassification::ProviderQuotaExhausted)
        }
        Some(PROVIDER_BILLING_BLOCKED) => {
            Some(AgentTaskFailureClassification::ProviderBillingBlocked)
        }
        Some(PROVIDER_CREDENTIALS_EXHAUSTED) => {
            Some(AgentTaskFailureClassification::ProviderCredentialsExhausted)
        }
        Some(PROVIDER_ACCOUNT_BLOCKED) => {
            Some(AgentTaskFailureClassification::ProviderAccountBlocked)
        }
        _ => None,
    }
}

/// The bounded tail of a provider runtime stream.
pub struct RuntimeStreamTail {
    /// Raw tail text. Only the adapter normalization consumes it, and that
    /// normalization redacts its output, so secrets never leave this struct
    /// unredacted.
    pub raw: String,
    /// Redacted tail text, used for every projected excerpt.
    pub redacted: String,
    pub truncated: bool,
}

/// Read the bounded tail of a provider runtime stream. Symlinks are refused:
/// a runtime stream is a Homeboy-captured or provider-written log file, and
/// following links here would let evidence content point anywhere.
pub fn read_runtime_stream_tail(path: &Path) -> Option<RuntimeStreamTail> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let len = metadata.len();
    let mut file = fs::File::open(path).ok()?;
    let start = len.saturating_sub(RUNTIME_STREAM_TAIL_BYTES as u64);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut bytes = Vec::with_capacity(RUNTIME_STREAM_TAIL_BYTES.min(len as usize));
    file.take(RUNTIME_STREAM_TAIL_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // A tail can start mid-line; drop the partial first line so only whole
    // events remain parseable.
    let text = if start > 0 {
        text.find('\n')
            .map(|index| text[index + 1..].to_string())
            .unwrap_or_default()
    } else {
        text
    };
    Some(RuntimeStreamTail {
        redacted: RedactionPolicy::default().redact_string(&text),
        raw: text,
        truncated: start > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_STREAM: &str = concat!(
        r#"{"type":"session.start","sessionID":"ses_fixture"}"#,
        "\n",
        r#"{"type":"error","error":{"name":"APIError","data":{"message":"personal-team-blocked:spending-limit: You have run out of credits or need a Grok subscription.","statusCode":403,"isRetryable":false}}}"#,
        "\n",
    );

    #[test]
    fn normalizes_the_live_opencode_account_rejection() {
        let normalized =
            normalize_runtime_stream_error(Some("opencode"), LIVE_STREAM).expect("normalized");

        assert_eq!(normalized["schema"], PROVIDER_STRUCTURED_ERROR_SCHEMA);
        assert_eq!(
            normalized["message"],
            "personal-team-blocked:spending-limit: You have run out of credits or need a Grok subscription."
        );
        assert_eq!(normalized["status_code"], 403);
        assert_eq!(normalized["retryable"], false);
        assert_eq!(
            normalized["failure_classification"],
            PROVIDER_BILLING_BLOCKED
        );
        assert_eq!(normalized["error_name"], "APIError");
    }

    #[test]
    fn normalizes_without_a_backend_hint_by_trying_registered_adapters() {
        let normalized = normalize_runtime_stream_error(None, LIVE_STREAM).expect("normalized");
        assert_eq!(normalized["status_code"], 403);
    }

    #[test]
    fn an_unregistered_backend_is_never_parsed() {
        assert!(normalize_runtime_stream_error(Some("claude-cli"), LIVE_STREAM).is_none());
    }

    #[test]
    fn the_last_error_event_wins() {
        let stream = concat!(
            r#"{"type":"error","error":{"data":{"message":"transient blip","statusCode":500,"isRetryable":true}}}"#,
            "\n",
            r#"{"type":"error","error":{"data":{"message":"final failure","statusCode":403,"isRetryable":false}}}"#,
            "\n",
        );
        let normalized =
            normalize_runtime_stream_error(Some("opencode"), stream).expect("normalized");
        assert_eq!(normalized["message"], "final failure");
    }

    #[test]
    fn a_retryable_error_classifies_as_rate_limited() {
        let normalized = normalize_runtime_stream_error(
            Some("opencode"),
            r#"{"type":"error","error":{"data":{"message":"too many requests","statusCode":429,"isRetryable":true}}}"#,
        )
        .expect("normalized");
        assert_eq!(normalized["failure_classification"], PROVIDER_RATE_LIMITED);
    }

    #[test]
    fn a_forbidden_resource_without_billing_vocabulary_is_a_generic_provider_error() {
        let normalized = normalize_runtime_stream_error(
            Some("opencode"),
            r#"{"type":"error","error":{"data":{"message":"repository not accessible","statusCode":403,"isRetryable":false}}}"#,
        )
        .expect("normalized");
        assert_eq!(normalized["failure_classification"], PROVIDER_ERROR);
    }

    #[test]
    fn distinguishes_permanent_quota_billing_and_credential_rejections() {
        for (message, expected) in [
            ("usage quota exhausted", PROVIDER_QUOTA_EXHAUSTED),
            ("billing payment required", PROVIDER_BILLING_BLOCKED),
            ("API key expired", PROVIDER_CREDENTIALS_EXHAUSTED),
        ] {
            assert_eq!(
                structured_error_failure_classification(message, Some(403), Some(false)),
                expected
            );
        }
    }

    #[test]
    fn non_error_streams_do_not_normalize() {
        assert!(normalize_runtime_stream_error(
            Some("opencode"),
            r#"{"type":"step_start","part":{"id":"p1"}}"#
        )
        .is_none());
        assert!(
            normalize_runtime_stream_error(Some("opencode"), "unstructured output\n").is_none()
        );
        // An error event without a message is not actionable evidence.
        assert!(normalize_runtime_stream_error(
            Some("opencode"),
            r#"{"type":"error","error":{"data":{"statusCode":500}}}"#
        )
        .is_none());
    }

    #[test]
    fn normalized_output_is_redacted() {
        let normalized = normalize_runtime_stream_error(
            Some("opencode"),
            r#"{"type":"error","error":{"data":{"message":"rejected for api_key=sk-live-secret-value","statusCode":401,"isRetryable":false}}}"#,
        )
        .expect("normalized");
        let serialized = normalized.to_string();
        assert!(!serialized.contains("sk-live-secret-value"));
        assert!(serialized.contains("[REDACTED]"));
    }

    #[test]
    fn the_recognizer_accepts_only_the_normalized_schema_with_a_message() {
        let normalized =
            normalize_runtime_stream_error(Some("opencode"), LIVE_STREAM).expect("normalized");
        assert!(normalized_structured_error(&normalized).is_some());
        assert!(normalized_structured_error(&json!({
            "schema": PROVIDER_STRUCTURED_ERROR_SCHEMA,
            "message": " ",
        }))
        .is_none());
        assert!(normalized_structured_error(&json!({
            "message": "vendor payload without the normalized schema",
        }))
        .is_none());
    }
}
