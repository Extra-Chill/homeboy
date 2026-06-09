use crate::core::error::{Error, ErrorCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunnerDaemonHealthFailure {
    pub reason: String,
    pub runner_id: Option<String>,
    pub job_id: Option<String>,
}

pub(super) fn runner_daemon_health_failure(err: &Error) -> Option<RunnerDaemonHealthFailure> {
    if !matches!(
        err.code,
        ErrorCode::InternalUnexpected | ErrorCode::InternalJsonError
    ) {
        return None;
    }

    let message = err.message.as_str();
    let daemon_transport_failure = message.contains("query runner daemon")
        || message.contains("submit runner daemon exec job")
        || message.contains("parse daemon exec response")
        || message.contains("daemon exec request failed");
    if daemon_transport_failure {
        Some(RunnerDaemonHealthFailure {
            reason: format!("runner daemon health check failed: {message}"),
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
    fn classifies_stale_daemon_transport_failures() {
        let err = Error::internal_unexpected(
            "query runner daemon: error sending request for url (http://127.0.0.1:63534/jobs/id)",
        );

        assert_eq!(
            runner_daemon_health_failure(&err),
            Some(RunnerDaemonHealthFailure {
                reason: "runner daemon health check failed: query runner daemon: error sending request for url (http://127.0.0.1:63534/jobs/id)"
                    .to_string(),
                runner_id: None,
                job_id: None,
            })
        );
    }

    #[test]
    fn includes_in_flight_daemon_job_context() {
        let err = Error::new(
            ErrorCode::InternalUnexpected,
            "query runner daemon: error sending request for url (http://127.0.0.1:63534/jobs/id)",
            serde_json::json!({
                "runner_id": "homeboy-lab",
                "job_id": "job-123",
            }),
        );

        assert_eq!(
            runner_daemon_health_failure(&err),
            Some(RunnerDaemonHealthFailure {
                reason: "runner daemon health check failed: query runner daemon: error sending request for url (http://127.0.0.1:63534/jobs/id)"
                    .to_string(),
                runner_id: Some("homeboy-lab".to_string()),
                job_id: Some("job-123".to_string()),
            })
        );
    }

    #[test]
    fn does_not_classify_unrelated_internal_errors_as_daemon_health() {
        let err = Error::internal_unexpected("workspace sync failed unexpectedly");

        assert_eq!(runner_daemon_health_failure(&err), None);
    }
}
