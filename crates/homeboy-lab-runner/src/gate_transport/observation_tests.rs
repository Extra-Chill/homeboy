use super::*;
use std::cell::Cell;

fn lost_transport() -> Error {
    Error::new(
        homeboy_core::ErrorCode::InternalUnexpected,
        "query runner daemon failed",
        serde_json::json!({"daemon_transport_error":{"kind":"connect"}}),
    )
}

fn deadline() -> Error {
    let mut error = Error::internal_unexpected("gate wait ran out of budget");
    error.details = serde_json::json!({"daemon_terminal_wait_timeout":{}});
    error
}

/// Only a controller-chosen stop (supervision cancel, heartbeat failure) or
/// the gate's own deadline cancels the remote job. A lost control plane is an
/// observation gap: the running gate is left alone and the failure is a
/// retryable deferral that names the job (#17376).
#[test]
fn only_a_controller_stop_cancels_a_running_gate() {
    struct Case {
        error: Error,
        stop: bool,
        cancel_result: Result<()>,
        cancels: bool,
        retryable: Option<bool>,
    }
    let case = |error, stop, cancel_result, cancels, retryable| Case {
        error,
        stop,
        cancel_result,
        cancels,
        retryable,
    };
    let cases = [
        case(lost_transport(), false, Ok(()), false, Some(true)),
        case(
            Error::invalid_argument("runner", "daemon generation mismatch"),
            false,
            Ok(()),
            false,
            None,
        ),
        case(
            Error::internal_unexpected("cancelled"),
            true,
            Ok(()),
            true,
            None,
        ),
        case(deadline(), false, Ok(()), true, None),
        // A cancel the controller chose, but that cannot reach the runner,
        // is still a retryable deferral rather than an invalid argument.
        case(
            Error::internal_unexpected("cancelled"),
            true,
            Err(crate::execution::runner_job_cancel_not_connected(
                "homeboy-lab",
            )),
            true,
            Some(true),
        ),
    ];
    for Case {
        error,
        stop,
        cancel_result,
        cancels,
        retryable,
    } in cases
    {
        let cancelled = Cell::new(false);
        let cancel_failed = cancel_result.is_err();
        let result = settle_gate_observation_failure(
            error,
            "gate-job",
            stop,
            || {
                cancelled.set(true);
                cancel_result
            },
            || Ok(()),
        );
        assert_eq!(cancelled.get(), cancels);
        if cancels && !cancel_failed {
            result.expect("an acknowledged cancel settles the gate");
            continue;
        }
        let error = result.expect_err("an unsettled gate reports its observation failure");
        assert_eq!(error.retryable, retryable);
        assert_eq!(error.details["job_id"], "gate-job");
        if retryable == Some(true) {
            let public = lab_gate_public_error(&error, "artifact".into());
            assert_eq!(public.details["gate_disposition"], "unavailable");
        }
    }
}
