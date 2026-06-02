use crate::core::error::{Error, ErrorCode};

pub(super) fn runner_daemon_health_failure(err: &Error) -> Option<String> {
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
        Some(format!("runner daemon health check failed: {message}"))
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
            Some(
                "runner daemon health check failed: query runner daemon: error sending request for url (http://127.0.0.1:63534/jobs/id)"
                    .to_string()
            )
        );
    }

    #[test]
    fn does_not_classify_unrelated_internal_errors_as_daemon_health() {
        let err = Error::internal_unexpected("workspace sync failed unexpectedly");

        assert_eq!(runner_daemon_health_failure(&err), None);
    }
}
