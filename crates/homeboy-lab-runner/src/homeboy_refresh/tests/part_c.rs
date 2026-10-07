#![cfg(test)]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::*;
use crate::{
    RollingDrainState, RunnerActiveJobState, RunnerDaemonGenerationStatus, RunnerExecMode,
    RunnerSessionState, RunnerStatusReport,
};
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use homeboy_core::error::Error;

#[test]
fn admission_reconciliation_reports_its_phase_before_the_probe() {
    homeboy_core::test_support::with_isolated_home(|_| {
        begin_refresh_live_progress("prepare");
        let guard = RefreshLiveGuard;
        let result = reconciled_refresh_admission_with(
            "lab",
            |_| {
                assert_eq!(refresh_live_progress().unwrap().sub_phase, "reconcile");
                Err(Error::internal_unexpected("probe failed".to_string()))
            },
            |_| unreachable!("a failed probe cannot project admission"),
        );
        assert!(result.is_err());
        drop(guard);
        assert!(refresh_live_progress().is_none());
    });
}

fn readiness_report(freshness: DaemonFreshnessReport) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: "homeboy-lab".to_string(),
        connected: true,
        state: RunnerSessionState::Connected,
        session: None,
        stale_daemon: None,
        configured_job_binary_build_identity: None,
        daemon_freshness: Some(freshness),
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "test".to_string(),
    }
}

fn freshness(
    fresh: bool,
    stale_reason_code: Option<DaemonStaleReasonCode>,
) -> DaemonFreshnessReport {
    DaemonFreshnessReport {
        fresh,
        stale_reason_code,
        restartable: true,
        lease_id: Some("lease-new".to_string()),
        pid: Some(1234),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: Vec::new(),
    }
}

fn generation(
    name: &str,
    admission_owner: bool,
    active_job_count: usize,
) -> RunnerDaemonGenerationStatus {
    RunnerDaemonGenerationStatus {
        generation: name.to_string(),
        admission_owner,
        drain_state: if admission_owner {
            RollingDrainState::Admitting
        } else {
            RollingDrainState::Draining
        },
        active_job_count,
        observed_active_job_count: Some(active_job_count),
        active_job_count_authoritative: true,
        job_owner_count: active_job_count,
        run_owner_count: 0,
        artifact_owner_count: 0,
        homeboy_build_identity: None,
        remote_daemon_lease_id: Some(name.to_string()),
        remote_daemon_address: None,
        local_url: None,
    }
}

#[test]
fn refresh_readiness_reports_active_draining_generation_with_its_exact_owner() {
    let mut report = readiness_report(freshness(true, None));
    report.active_job_count = 1;
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &report,
        &[
            generation("lease-old", false, 1),
            generation("lease-new", true, 0),
        ],
        &[],
    );

    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Draining);
    assert_eq!(readiness.owners, ["lease-old"]);
    assert_eq!(
        readiness.continuation.as_deref(),
        Some("homeboy runner reconcile homeboy-lab")
    );
}

#[test]
fn refresh_readiness_blocks_a_post_refresh_binary_hash_mismatch() {
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &readiness_report(freshness(
            false,
            Some(DaemonStaleReasonCode::BinaryHashMismatch),
        )),
        &[generation("lease-new", true, 0)],
        &[],
    );

    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Blocked);
    assert!(!readiness.daemon_fresh);
    assert!(!readiness.accepting_jobs);
    assert_eq!(readiness.owners, ["lease-new"]);
    // This fixture carries no typed repair plan, and a stale report without one
    // is terminal even when its ownership proof is sufficient (#12797): there is
    // no authorized mutation to prescribe. The readiness therefore falls back to
    // the read-only status projection rather than a `runner doctor` action.
    assert_eq!(
        readiness.continuation.as_deref(),
        Some("homeboy runner status homeboy-lab --full")
    );
}

#[test]
fn refresh_readiness_certifies_immediate_admission_only_when_ready() {
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &readiness_report(freshness(true, None)),
        &[generation("lease-new", true, 0)],
        &[],
    );

    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Ready);
    assert!(readiness.daemon_fresh);
    assert!(readiness.accepting_jobs);
    assert!(readiness.continuation.is_none());
}

#[test]
fn incomplete_readiness_does_not_rewrite_daemon_refresh_compatibility_facts() {
    let mut report = readiness_report(freshness(true, None));
    report.active_job_count = 1;
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &report,
        &[
            generation("lease-old", false, 1),
            generation("lease-new", true, 0),
        ],
        &[],
    );

    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Draining);
    // A completed rotation remains a completed daemon operation. The non-ready
    // readiness state and nonzero refresh result express incomplete convergence.
    assert!(!reconnect_required_after_refresh(true));
    assert!(reconnect_required_after_refresh(false));
    assert_eq!(
        readiness.continuation.as_deref(),
        Some("homeboy runner reconcile homeboy-lab")
    );
}

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
    crate::daemon_http_get::daemon_transport_error(
        crate::daemon_http_get::DaemonHttpErrorKind::Connect,
        "/admissions",
        None,
        "runner endpoint unavailable",
        "connection refused",
    )
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

fn unconverged_refresh_fixture() -> (HomeboyBinaryRefreshPlan, RunnerExecOutput) {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "homeboy-lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/runner/homeboy".to_string(),
        script: "fixture".to_string(),
        reconnect: true,
        followup_commands: Vec::new(),
    };
    let execution = RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "homeboy-lab".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: Vec::new(),
        remote_cwd: "/runner".to_string(),
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: None,
        job_events: None,
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    };
    (plan, execution)
}

/// #15087: every named phase (materialize/select, promotion, disconnect,
/// reconnect, daemon identity verification, admission readiness) can report
/// `succeeded` while the runner's own admission postcondition still refuses
/// to converge. That must never surface as `failure: None` alongside a
/// non-zero exit — the exact "exited 1 without reporting a failure cause"
/// this issue reported.
#[test]
fn unconverged_readiness_produces_a_named_failure_not_a_silent_exit() {
    let (plan, execution) = unconverged_refresh_fixture();
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &readiness_report(freshness(
            false,
            Some(DaemonStaleReasonCode::VersionMismatch),
        )),
        &[generation("lease-new", true, 0)],
        &[],
    );
    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Blocked);

    let failure = unconverged_refresh_failure(&plan, &execution, false, Some(&readiness))
        .expect("a non-converged reconnect must carry a named failure cause");
    let message = failure
        .verification
        .expect("unconverged failure carries a verification message");
    assert!(
        message.contains("did not converge"),
        "failure must explain that the reconnect did not converge: {message}"
    );
    assert!(
        message.contains("Blocked"),
        "failure must name the readiness state that blocked convergence: {message}"
    );
}

/// A converged reconnect (readiness `Ready`, or no reconnect at all) must
/// never synthesize a failure — this exercises both branches side by side so
/// a future edit cannot silently start reporting spurious failures on the
/// success path.
#[test]
fn converged_or_absent_readiness_never_synthesizes_a_failure() {
    let (plan, execution) = unconverged_refresh_fixture();
    let ready = refresh_readiness_from_status(
        "homeboy-lab",
        &readiness_report(freshness(true, None)),
        &[generation("lease-new", true, 0)],
        &[],
    );
    assert_eq!(ready.state, HomeboyRefreshReadinessState::Ready);

    assert!(unconverged_refresh_failure(&plan, &execution, true, Some(&ready)).is_none());
    assert!(unconverged_refresh_failure(&plan, &execution, true, None).is_none());
}

/// The rendered message must be actionable on its own: name the admission
/// state, whether jobs are accepted, daemon freshness, the exact owners
/// fencing convergence, and the one command that resumes the lifecycle.
#[test]
fn unconverged_readiness_message_names_state_owners_and_next_action() {
    let readiness = refresh_readiness_from_status(
        "homeboy-lab",
        &{
            let mut report = readiness_report(freshness(true, None));
            report.active_job_count = 1;
            report
        },
        &[
            generation("lease-old", false, 1),
            generation("lease-new", true, 0),
        ],
        &[],
    );
    assert_eq!(readiness.state, HomeboyRefreshReadinessState::Draining);

    let message = unconverged_readiness_message("homeboy-lab", &readiness);
    assert!(message.contains("homeboy-lab"));
    assert!(message.contains("Draining"));
    assert!(message.contains("accepting_jobs=false"));
    assert!(message.contains("daemon_fresh=true"));
    assert!(message.contains("lease-old"), "must name the fencing owner");
    assert!(
        message.contains("homeboy runner reconcile homeboy-lab"),
        "must surface the exact continuation command"
    );
}
