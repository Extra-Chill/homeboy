use homeboy_core::error::{Error, ErrorCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunnerDaemonHealthFailure {
    pub reason: String,
    pub runner_id: Option<String>,
    pub job_id: Option<String>,
}

pub(crate) fn runner_daemon_health_failure(err: &Error) -> Option<RunnerDaemonHealthFailure> {
    if !matches!(
        err.code,
        ErrorCode::InternalUnexpected | ErrorCode::InternalJsonError
    ) {
        return None;
    }

    let daemon_transport_failure = err
        .details
        .pointer("/daemon_transport_error/kind")
        .and_then(|value| value.as_str())
        .is_some_and(|kind| {
            matches!(
                kind,
                "connect" | "timeout" | "status" | "request" | "body_decode"
            )
        });
    if daemon_transport_failure {
        Some(RunnerDaemonHealthFailure {
            reason: format!("runner daemon health check failed: {}", err.message),
            runner_id: err
                .details
                .get("runner_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            job_id: err
                .details
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_daemon_transport_failures_with_their_job_context() {
        let stale = Error::new(
            ErrorCode::InternalUnexpected,
            "runner daemon request failed",
            serde_json::json!({
                "daemon_transport_error": {
                    "kind": "connect",
                    "path": "/jobs/id",
                    "http_status": null,
                }
            }),
        );
        let in_flight = Error::new(
            ErrorCode::InternalUnexpected,
            "query runner daemon: connect failed",
            serde_json::json!({
                "runner_id": "homeboy-lab",
                "job_id": "job-123",
                "daemon_transport_error": { "kind": "connect" },
            }),
        );
        let unrelated =
            Error::internal_unexpected("query runner daemon: workspace sync failed unexpectedly");

        for (name, err, expected) in [
            (
                "stale transport without context",
                stale,
                Some(RunnerDaemonHealthFailure {
                    reason: "runner daemon health check failed: runner daemon request failed"
                        .to_string(),
                    runner_id: None,
                    job_id: None,
                }),
            ),
            (
                "in-flight job context",
                in_flight,
                Some(RunnerDaemonHealthFailure {
                    reason:
                        "runner daemon health check failed: query runner daemon: connect failed"
                            .to_string(),
                    runner_id: Some("homeboy-lab".to_string()),
                    job_id: Some("job-123".to_string()),
                }),
            ),
            ("unrelated internal error", unrelated, None),
        ] {
            assert_eq!(runner_daemon_health_failure(&err), expected, "{name}");
        }
    }
}
