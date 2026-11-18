//! Controller<->daemon request-shape skew (#17680).
//!
//! A newer CLI can submit a request the serving daemon's older build cannot
//! deserialize (`deny_unknown_fields` rejects a field the daemon has never
//! heard of). That is not an invalid definition and not an internal failure:
//! the daemon is stale relative to its submitter. Both sides classify it as
//! the typed `daemon.stale_capability` error naming both builds.

use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Hint};

/// Envelope field naming the submitter's build identity display.
pub const SUBMITTER_BUILD_FIELD: &str = "submitter_build";

/// Whether `error` is a request-shape deserialization refusal (an unknown
/// field or variant), the failure an older build gives a newer shape.
pub fn is_request_shape_error(error: &Error) -> bool {
    error.code == ErrorCode::ValidationInvalidArgument && is_request_shape_message(&error.message)
}

fn is_request_shape_message(message: &str) -> bool {
    message.contains("unknown field `") || message.contains("unknown variant `")
}

fn builds_differ(daemon_build: &str, submitter_build: &str) -> bool {
    let (daemon, submitter) = (daemon_build.trim(), submitter_build.trim());
    !daemon.is_empty() && !submitter.is_empty() && daemon != submitter
}

/// The typed stale-capability error naming both builds.
pub fn stale_capability_error(
    daemon_build: &str,
    submitter_build: &str,
    serde_message: &str,
    active_durable_jobs: Option<usize>,
) -> Error {
    let next_actions = vec![
        "Run: homeboy daemon status".to_string(),
        "The daemon replaces itself with the current build once it has no active durable jobs; retry after it does".to_string(),
    ];
    let mut error = Error::new(
        ErrorCode::DaemonStaleCapability,
        format!(
            "the serving daemon runs `{daemon_build}`, a different build than the submitter `{submitter_build}`, and cannot read this request: {serde_message}"
        ),
        json!({
            "daemon_version": daemon_build,
            "submitter_version": submitter_build,
            "serde_message": serde_message,
            "active_durable_jobs": active_durable_jobs,
            "next_actions": next_actions,
        }),
    );
    error.hints = next_actions
        .into_iter()
        .map(|message| Hint { message })
        .collect();
    error
}

/// Daemon side: reclassify an admission failure when the submitter runs a
/// different build and the request could not be deserialized. A same-build
/// (or unattributed) submitter keeps the original error unchanged.
pub fn classify_admission_error(
    error: Error,
    submitter_build: Option<&str>,
    daemon_build: &str,
    active_durable_jobs: impl FnOnce() -> Option<usize>,
) -> Error {
    match submitter_build {
        Some(submitter)
            if builds_differ(daemon_build, submitter) && is_request_shape_error(&error) =>
        {
            stale_capability_error(
                daemon_build,
                submitter,
                &error.message,
                active_durable_jobs(),
            )
        }
        _ => error,
    }
}

/// Client side: decide whether a failed submission response is a stale
/// capability. A daemon that already classified it is re-raised as typed; an
/// older daemon's invalid-argument request-shape refusal is rewrapped when
/// its build (`daemon_build`) differs from the CLI's (`cli_build`).
pub fn rewrap_submission_failure(
    response: &Value,
    daemon_build: Option<&str>,
    cli_build: &str,
) -> Option<Error> {
    let code = response.get("error").and_then(Value::as_str)?;
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let details = response.get("details");
    if code == ErrorCode::DaemonStaleCapability.as_str() {
        let field = |key: &str| {
            details
                .and_then(|details| details.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let active = details
            .and_then(|details| details.get("active_durable_jobs"))
            .and_then(Value::as_u64)
            .map(|count| count as usize);
        return Some(stale_capability_error(
            &field("daemon_version"),
            &field("submitter_version"),
            &field("serde_message"),
            active,
        ));
    }
    if code != ErrorCode::ValidationInvalidArgument.as_str() || !is_request_shape_message(message) {
        return None;
    }
    let daemon_build = daemon_build?;
    builds_differ(daemon_build, cli_build)
        .then(|| stale_capability_error(daemon_build, cli_build, message, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAEMON: &str = "homeboy 0.470.0+aaaaaaaa";
    const CLI: &str = "homeboy 0.474.7+bbbbbbbb";
    const SERDE: &str = "Invalid argument 'workflow': invalid workflow request: unknown field `next_pass`, expected one of `id`, `steps`";

    fn shape_error() -> Error {
        Error::validation_invalid_argument(
            "workflow",
            "invalid workflow request: unknown field `next_pass`, expected one of `id`, `steps`",
            None,
            None,
        )
    }

    #[test]
    fn different_build_unknown_field_is_typed_stale_capability() {
        let error = classify_admission_error(shape_error(), Some(CLI), DAEMON, || Some(2));
        assert_eq!(error.code, ErrorCode::DaemonStaleCapability);
        assert_eq!(error.code.as_str(), "daemon.stale_capability");
        assert_eq!(error.details["daemon_version"], DAEMON);
        assert_eq!(error.details["submitter_version"], CLI);
        assert_eq!(error.details["active_durable_jobs"], 2);
        assert!(error.details["serde_message"]
            .as_str()
            .unwrap()
            .contains("unknown field `next_pass`"));
        assert!(error.hints[0].message.contains("homeboy daemon status"));
    }

    #[test]
    fn same_build_keeps_invalid_argument() {
        let error = classify_admission_error(shape_error(), Some(DAEMON), DAEMON, || {
            panic!("active jobs are not read for a same-build submitter")
        });
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.message, shape_error().message);
    }

    #[test]
    fn unattributed_submitter_keeps_invalid_argument() {
        let error = classify_admission_error(shape_error(), None, DAEMON, || None);
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn different_build_non_shape_error_is_unchanged() {
        let error = classify_admission_error(
            Error::validation_invalid_argument("workflow", "digest mismatch", None, None),
            Some(CLI),
            DAEMON,
            || None,
        );
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn old_envelope_without_submitter_build_parses() {
        let request: super::super::ControllerJobRequest = serde_json::from_value(json!({
            "type": "workflow",
            "version": 1,
            "idempotency_key": "wf-x",
            "request": {},
        }))
        .unwrap();
        assert_eq!(request.submitter_build, None);
        let request: super::super::ControllerJobRequest = serde_json::from_value(json!({
            "type": "workflow",
            "version": 1,
            "idempotency_key": "wf-x",
            "request": {},
            SUBMITTER_BUILD_FIELD: CLI,
        }))
        .unwrap();
        assert_eq!(request.submitter_build.as_deref(), Some(CLI));
    }

    fn old_daemon_response() -> Value {
        json!({
            "error": "validation.invalid_argument",
            "message": SERDE,
            "details": { "field": "workflow" },
        })
    }

    #[test]
    fn client_rewraps_old_daemon_shape_refusal_when_builds_differ() {
        let error = rewrap_submission_failure(&old_daemon_response(), Some(DAEMON), CLI)
            .expect("rewrapped");
        assert_eq!(error.code, ErrorCode::DaemonStaleCapability);
        assert_eq!(error.details["daemon_version"], DAEMON);
        assert_eq!(error.details["submitter_version"], CLI);
    }

    #[test]
    fn client_does_not_rewrap_same_build_or_unknown_daemon_build() {
        assert!(rewrap_submission_failure(&old_daemon_response(), Some(CLI), CLI).is_none());
        assert!(rewrap_submission_failure(&old_daemon_response(), None, CLI).is_none());
        let other = json!({
            "error": "validation.invalid_argument",
            "message": "workflow definition digest does not match its body",
        });
        assert!(rewrap_submission_failure(&other, Some(DAEMON), CLI).is_none());
    }

    #[test]
    fn client_reraises_daemon_classified_stale_capability() {
        let daemon_error = stale_capability_error(DAEMON, CLI, SERDE, Some(1));
        let response = json!({
            "error": daemon_error.code.as_str(),
            "message": daemon_error.message,
            "details": daemon_error.details,
        });
        let error = rewrap_submission_failure(&response, None, CLI).expect("typed");
        assert_eq!(error.code, ErrorCode::DaemonStaleCapability);
        assert_eq!(error.details["active_durable_jobs"], 1);
        assert_eq!(error.details["daemon_version"], DAEMON);
    }
}
