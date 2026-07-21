#![cfg(test)]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::*;
use homeboy_core::error::Error;

fn not_fresh_error() -> Error {
    // Shape mirrors `reserve_daemon_admission` when the daemon refuses the
    // reservation because its just-rotated lease heartbeat is not yet fresh.
    Error::validation_invalid_argument(
        "runner",
        "runner `homeboy-lab` refused Lab admission reservation: daemon lease is not fresh",
        Some("homeboy-lab".to_string()),
        None,
    )
}

fn transport_drop_error() -> Error {
    // Shape mirrors a dropped first request against the new loopback tunnel.
    let mut error = Error::internal_unexpected(
        "query runner daemon: error sending request for url (http://127.0.0.1:52163/admissions)",
    );
    error.retryable = Some(true);
    error
}

fn lease_mismatch_error() -> Error {
    // Shape mirrors `reserve_daemon_admission` when the endpoint admitted
    // against a different lease than the one we expected — a different daemon
    // owns it, so retrying cannot converge.
    Error::validation_invalid_argument(
        "expected_daemon_lease_id",
        "runner `homeboy-lab` admitted against daemon lease `other`, expected `lease-new`",
        Some("lease-new".to_string()),
        None,
    )
}

/// #9466: a freshly reconnected daemon momentarily refuses admission (lease not
/// fresh) and can drop the first request. The readiness probe must retry
/// through that window and converge, so the refresh only reports success once
/// the daemon will actually admit the next Lab handoff.
#[test]
fn admission_readiness_retries_through_the_transient_reconnect_window() {
    let attempts = RefCell::new(0);
    let result = probe_admission_readiness_until_ready(
        "lease-new",
        Instant::now() + Duration::from_secs(30),
        || {
            let mut count = attempts.borrow_mut();
            *count += 1;
            match *count {
                1 => Err(not_fresh_error()),
                2 => Err(transport_drop_error()),
                _ => Ok(()),
            }
        },
        || {},
    );

    assert!(
        result.is_ok(),
        "probe must converge after the lease settles: {result:?}"
    );
    assert_eq!(
        *attempts.borrow(),
        3,
        "probe must retry the not-fresh lease and the transport drop before succeeding"
    );
}

/// An authoritative lease mismatch means a different daemon owns the admission
/// endpoint; no amount of waiting will converge it, so the probe must fail
/// immediately without burning the readiness window.
#[test]
fn admission_readiness_fails_immediately_on_an_authoritative_lease_mismatch() {
    let attempts = RefCell::new(0);
    let result = probe_admission_readiness_until_ready(
        "lease-new",
        Instant::now() + Duration::from_secs(30),
        || {
            *attempts.borrow_mut() += 1;
            Err(lease_mismatch_error())
        },
        || panic!("an authoritative mismatch must not wait and retry"),
    );

    let error = result.expect_err("lease mismatch is authoritative");
    assert_eq!(error.details["field"], "expected_daemon_lease_id");
    assert_eq!(
        *attempts.borrow(),
        1,
        "authoritative mismatch must not retry"
    );
}

/// When the daemon never becomes ready within the window, the probe surfaces a
/// single canonical recovery action so the operator has exactly one next step.
#[test]
fn admission_readiness_timeout_surfaces_one_canonical_recovery_action() {
    let result = probe_admission_readiness_until_ready(
        "lease-new",
        Instant::now(),
        || Err(not_fresh_error()),
        || panic!("an already-expired deadline must not wait"),
    );

    let error = result.expect_err("an unready daemon must fail the refresh");
    assert_eq!(error.details["field"], "reconnect");
    assert!(
        error.message.contains("did not become ready to admit"),
        "timeout must explain the readiness failure: {}",
        error.message
    );
    let tried = error.details["tried"]
        .as_array()
        .expect("timeout error carries a recovery action");
    assert_eq!(tried.len(), 1, "exactly one canonical recovery action");
    assert!(
        tried[0]
            .as_str()
            .expect("recovery action string")
            .contains("refresh-homeboy"),
        "recovery action points at the refresh command: {tried:?}"
    );
    // The underlying transient error is preserved for diagnosis.
    assert!(error.message.contains("daemon lease is not fresh"));
}

/// The retry classifier treats only an authoritative lease mismatch as fatal;
/// both #9466 transient shapes (not-fresh lease and transport drop) are
/// retryable so the reconnect can converge.
#[test]
fn only_lease_mismatch_is_treated_as_authoritative() {
    assert!(admission_readiness_failure_is_authoritative(
        &lease_mismatch_error()
    ));
    assert!(!admission_readiness_failure_is_authoritative(
        &not_fresh_error()
    ));
    assert!(!admission_readiness_failure_is_authoritative(
        &transport_drop_error()
    ));
}
