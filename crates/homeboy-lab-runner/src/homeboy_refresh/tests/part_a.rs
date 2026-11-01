#![cfg(test)]

use super::*;
use crate::{
    RunnerActiveJobState, RunnerAdmissionSnapshot, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStatusReport,
};
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use std::cell::RefCell;

/// Environment variables that redirect Cargo's output directory. A nested Cargo
/// fixture that asserts a fixture-local `target/` path must not inherit these
/// from the controller, `homeboy review`, or CI — otherwise the nested build
/// succeeds while writing somewhere else entirely, and the assertion fails on a
/// path that was never going to exist (#9846).
#[test]
fn routine_reconnect_refuses_to_interrupt_an_admitted_lab_offload() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let error = plan_daemon_restart("homeboy-lab", &[admission], false)
        .expect_err("a job the restarted daemon cannot re-adopt defers the reconnect");

    assert_eq!(
        error.details["active_job_ids"],
        serde_json::json!(["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"])
    );
    assert!(error.message.contains("homeboy-lab"));
    assert!(error.message.contains("--force"));
    assert!(error.details["tried"][0]
        .as_str()
        .is_some_and(|command| command.contains("runner job logs homeboy-lab")));
}

#[test]
fn forced_reconnect_reports_the_jobs_it_will_interrupt() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let mut re_adoptable = active_admission("6f1c1f5e-0d8e-4a43-9b1f-0c7d1f0a9b11");
    re_adoptable.re_adoptable = true;
    let restart = plan_daemon_restart("homeboy-lab", &[admission, re_adoptable], true)
        .expect("explicit force permits interruption");

    assert_eq!(
        restart.interrupted_job_ids(),
        vec![
            "0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string(),
            "6f1c1f5e-0d8e-4a43-9b1f-0c7d1f0a9b11".to_string(),
        ],
        "--force interrupts every active job, re-adoptable or not"
    );
}

#[test]
fn re_adoptable_active_jobs_restart_the_daemon_in_place() {
    let mut exec_job = active_admission("exec-job");
    exec_job.re_adoptable = true;
    let mut step_job = active_admission("step-job");
    step_job.re_adoptable = true;

    let restart = plan_daemon_restart("homeboy-lab", &[exec_job, step_job], false)
        .expect("re-adoptable work never defers the reconnect");

    assert_eq!(restart, DaemonRestart::InPlace);
    assert!(
        restart.interrupted_job_ids().is_empty(),
        "an in-place restart interrupts nothing"
    );
    assert_eq!(
        plan_daemon_restart("homeboy-lab", &[], false).expect("idle runner"),
        DaemonRestart::InPlace,
        "an idle runner restarts in place too"
    );
}

/// #16680: with a connected daemon and one active re-adoptable exec job,
/// refresh takes the `restart_in_place` route, which restarts the daemon on
/// its state dir and rebinds the job's owner. It does not take disconnect
/// plus connect, and for a service-managed runner it never defers and never
/// interrupts the job. The restart itself is covered end to end in
/// `execution::tests::restart_in_place`.
#[test]
fn re_adoptable_active_job_routes_refresh_through_restart_in_place() {
    let session = crate::test_support::direct_ssh_session("lease-old");
    let mut exec_job = active_admission("exec-job");
    exec_job.re_adoptable = true;

    for service_managed in [false, true] {
        let restart = plan_daemon_restart("homeboy-lab", std::slice::from_ref(&exec_job), false)
            .expect("a re-adoptable job never defers the refresh");
        assert!(
            restart.interrupted_job_ids().is_empty(),
            "service_managed={service_managed}: the job is not interrupted"
        );
        assert_eq!(
            refresh_reconnect_route(&restart, Some(&session), service_managed),
            RefreshReconnectRoute::RestartInPlace(&session),
            "service_managed={service_managed}: restart in place, not disconnect plus connect"
        );
    }
}

/// The negative case. A job the restarted daemon cannot re-adopt defers the
/// refresh before any route is taken, so the daemon is neither stopped nor
/// disconnected. `--force` takes the disconnect route instead and names the
/// job it interrupts.
#[test]
fn non_re_adoptable_job_defers_refresh_and_force_interrupts_it() {
    let session = crate::test_support::direct_ssh_session("lease-old");
    let queued = active_admission("queued-job");

    let deferred = plan_daemon_restart("homeboy-lab", std::slice::from_ref(&queued), false)
        .expect_err("a non-re-adoptable job defers the refresh");
    assert_eq!(
        deferred.details["active_job_ids"],
        serde_json::json!(["queued-job"])
    );

    for service_managed in [false, true] {
        let forced = plan_daemon_restart("homeboy-lab", std::slice::from_ref(&queued), true)
            .expect("--force permits the interruption");
        assert_eq!(forced.interrupted_job_ids(), vec!["queued-job".to_string()]);
        assert_eq!(
            refresh_reconnect_route(&forced, Some(&session), service_managed),
            RefreshReconnectRoute::Reconnect {
                restart_service: service_managed,
                disconnect: Some(&session),
            },
            "service_managed={service_managed}: --force disconnects the connected daemon"
        );
    }
    assert_eq!(
        refresh_reconnect_route(&DaemonRestart::InPlace, None, false),
        RefreshReconnectRoute::Reconnect {
            restart_service: false,
            disconnect: None,
        },
        "with no connected daemon there is nothing to restart in place"
    );
}

#[test]
fn one_job_that_cannot_be_re_adopted_defers_the_whole_restart() {
    let mut exec_job = active_admission("exec-job");
    exec_job.re_adoptable = true;
    let queued = active_admission("queued-job");

    let error = plan_daemon_restart("homeboy-lab", &[exec_job, queued], false)
        .expect_err("a non-re-adoptable job keeps the drain/defer behaviour");

    assert_eq!(
        error.details["active_job_ids"],
        serde_json::json!(["queued-job"]),
        "only the job that blocks the restart is named"
    );
}

#[test]
fn deferred_reconnect_reports_selected_binary_and_exact_active_job_followups() {
    let job_id = "0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string();
    let followups = active_job_followups("homeboy-lab", std::slice::from_ref(&job_id));

    let deferred = HomeboyReconnectDeferred {
        reason: "active_daemon_jobs",
        active_job_ids: vec![job_id.clone()],
        selected_binary_path: "/runner/homeboy".to_string(),
        followup_commands: followups.clone(),
        ownership_contention: None,
    };

    assert_eq!(deferred.active_job_ids, vec![job_id]);
    assert_eq!(deferred.selected_binary_path, "/runner/homeboy");
    assert_eq!(
        followups,
        vec![
            "homeboy runner job logs homeboy-lab 0b77251a-b6a7-42a6-91a3-e49ff5f57c16 --follow",
            "homeboy runner refresh-homeboy homeboy-lab --reconnect",
        ]
    );
}

fn active_admission(job_id: &str) -> homeboy_core::api_jobs::ActiveRunnerJobSummary {
    homeboy_core::api_jobs::ActiveRunnerJobSummary {
        runner_id: "homeboy-lab".to_string(),
        job_id: job_id.to_string(),
        operation: "runner.admission".to_string(),
        source: "runner-daemon".to_string(),
        kind: "runner.admission".to_string(),
        status: homeboy_core::api_jobs::JobStatus::Queued,
        command: "homeboy generic-workload".to_string(),
        cwd: None,
        started_at_ms: 1,
        updated_at_ms: 1,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: Default::default(),
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: None,
        stale_reason: None,
        active_child_count: None,
        active_cell_count: None,
        re_adoptable: false,
    }
}

#[test]
fn refresh_preserves_only_its_direct_controller_lease_for_orphan_recovery() {
    let session = RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:7421".to_string()),
        local_port: Some(7421),
        local_url: Some("http://127.0.0.1:7421".to_string()),
        tunnel_pid: Some(1),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("lease-refresh".to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };

    assert_eq!(
        refresh_owned_lease(session),
        Some("lease-refresh".to_string())
    );
}

#[test]
fn disconnected_refresh_connects_without_rotating_a_missing_daemon() {
    let operations = std::cell::RefCell::new(Vec::new());

    disconnect_before_reconnect(None, |_| {
        panic!("a disconnected runner has no daemon to rotate")
    })
    .expect("disconnected refresh does not attempt daemon rotation");
    operations.borrow_mut().push("connect promoted binary");

    assert_eq!(operations.into_inner(), ["connect promoted binary"]);
}

/// #12418: reconnect behavior is selected from the authoritative post-lease
/// transport state. A stale but connected daemon is still rotated; a retained
/// record for a disconnected runner must not block connecting the promotion.
#[test]
fn reconnect_transport_state_matrix_preserves_disconnected_and_rotation_paths() {
    let disconnected = refreshed_daemon_status(false, None);
    let mut stale_connected = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale_connected.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+newcommit".to_string()),
    ));
    let healthy_connected = refreshed_daemon_status(true, Some("homeboy 0.294.0+newcommit"));

    assert!(
        !reconnect_restarts_existing_transport(&disconnected),
        "a disconnected runner starts the promoted binary directly"
    );
    assert_eq!(
        reconnect_transport_exit_code(disconnected.is_connected()),
        1,
        "a disconnected runner cannot report reconnect transport success"
    );
    assert!(
        reconnect_restarts_existing_transport(&stale_connected),
        "a stale connected daemon is rotated before reconnect"
    );
    assert_eq!(
        reconnect_transport_exit_code(stale_connected.is_connected()),
        0,
        "a stale but connected transport remains a completed reconnect phase"
    );
    assert!(
        reconnect_restarts_existing_transport(&healthy_connected),
        "a healthy connected daemon retains the established rotation behavior"
    );
    assert_eq!(
        reconnect_transport_exit_code(healthy_connected.is_connected()),
        0,
        "a healthy connected transport remains a completed reconnect phase"
    );
}

#[test]
fn connected_refresh_rotates_before_connecting_the_promoted_binary() {
    let session = RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:7421".to_string()),
        local_port: Some(7421),
        local_url: Some("http://127.0.0.1:7421".to_string()),
        tunnel_pid: Some(1),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("lease-refresh".to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };
    let operations = std::cell::RefCell::new(Vec::new());

    disconnect_before_reconnect(Some(&session), |_| {
        operations.borrow_mut().push("disconnect existing daemon");
        Ok(())
    })
    .expect("connected refresh rotates the existing daemon");
    operations.borrow_mut().push("connect promoted binary");

    assert_eq!(
        operations.into_inner(),
        ["disconnect existing daemon", "connect promoted binary"]
    );
}

/// #12418: materialization begins from a disconnected observation, but another
/// controller can connect the old daemon before this refresh obtains promotion
/// ownership. The post-lease session must drive retirement, and re-adoptable
/// active work restarts in place rather than being interrupted.
#[test]
fn materialization_race_restarts_the_newly_connected_daemon_and_preserves_active_work() {
    let materialization_snapshot = refreshed_daemon_status(false, None);
    let mut post_lease_status = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    let old_session = post_lease_status
        .session
        .as_mut()
        .expect("post-materialization connection persists a session");
    old_session.remote_daemon_lease_id = Some("lease-old".to_string());
    old_session.remote_daemon_pid = Some(42);

    assert!(
        !reconnect_restarts_existing_transport(&materialization_snapshot),
        "materialization began while the runner was disconnected"
    );
    let post_lease_session = reconnect_session_after_promotion(true, &post_lease_status)
        .expect("the post-lease connected daemon is the one to retire");
    assert_eq!(
        post_lease_session.remote_daemon_lease_id.as_deref(),
        Some("lease-old")
    );

    let operations = RefCell::new(Vec::new());
    disconnect_before_reconnect(Some(&post_lease_session), |_| {
        operations.borrow_mut().push("retire lease-old");
        Ok(())
    })
    .expect("the daemon observed after materialization is retired before connect");
    operations.borrow_mut().push("connect promoted binary");
    assert_eq!(
        operations.into_inner(),
        ["retire lease-old", "connect promoted binary"]
    );

    let mut active_job = active_admission("active-job");
    active_job.re_adoptable = true;
    assert_eq!(
        plan_daemon_restart("lab", std::slice::from_ref(&active_job), false)
            .expect("re-adoptable work restarts in place"),
        DaemonRestart::InPlace,
        "active re-adoptable work restarts the daemon in place instead of interrupting it"
    );
}

fn refreshed_daemon_status(connected: bool, identity: Option<&str>) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: "lab".to_string(),
        connected,
        state: if connected {
            RunnerSessionState::Connected
        } else {
            RunnerSessionState::Disconnected
        },
        session: identity.map(|homeboy_build_identity| RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(homeboy_build_identity.to_string()),
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }),
        stale_daemon: None,
        configured_job_binary_build_identity: identity.map(str::to_string),
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: String::new(),
    }
}

#[test]
fn refreshed_daemon_verification_accepts_the_post_start_health_window() {
    let not_ready = refreshed_daemon_status(false, Some("homeboy 0.1.0+06bbf46013cf"));
    let ready = refreshed_daemon_status(true, Some("homeboy 0.1.0+06bbf46013cf"));
    let mut statuses = [not_ready, ready].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "06bbf46013cf",
        None,
        || Ok(statuses.next().expect("post-start status probe")),
        || retries += 1,
    )
    .expect("the persisted connected session identifies the requested daemon commit");
    assert_eq!(retries, 1, "the initial tunnel health race is retried once");
}

#[test]
fn refreshed_daemon_verification_converges_after_exact_identity_refresh() {
    let mut stale = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+19a41cd5102d".to_string()),
    ));
    let converged = refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    let mut statuses = [stale, converged].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "19a41cd5102d",
        None,
        || Ok(statuses.next().expect("refresh convergence status")),
        || retries += 1,
    )
    .expect("refresh converges once daemon and configured executable identities match");
    assert_eq!(retries, 1);
}

#[test]
fn refreshed_daemon_verification_retries_until_the_configured_job_binary_converges() {
    let daemon_identity = "homeboy 0.294.0+19a41cd5102d";
    let mut stale_configured = refreshed_daemon_status(true, Some(daemon_identity));
    stale_configured.configured_job_binary_build_identity =
        Some("homeboy 0.294.0+oldcommit".to_string());
    let converged = refreshed_daemon_status(true, Some(daemon_identity));
    let mut statuses = [stale_configured, converged].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "19a41cd5102d",
        None,
        || Ok(statuses.next().expect("topology status probe")),
        || retries += 1,
    )
    .expect("refresh succeeds only after the configured binary and daemon converge");

    assert_eq!(
        retries, 1,
        "the stale configured binary is reread before success"
    );
}

#[test]
fn refreshed_daemon_verification_rejects_a_persisted_configured_binary_mismatch() {
    let mut status = refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    status.configured_job_binary_build_identity = Some("homeboy 0.294.0+oldcommit".to_string());

    let error = verify_refreshed_daemon_topology_status("lab", "19a41cd5102d", None, &status)
        .expect_err("matching daemon alone must not certify refresh success");

    assert!(error.message.contains("retained configured job binary"));
}

#[test]
fn refreshed_daemon_verification_rejects_commit_substring_mismatch() {
    let status = refreshed_daemon_status(true, Some("homeboy 0.1.0+x06bbf46013cf"));

    let error = verify_refreshed_daemon_topology_status("lab", "06bbf46013cf", None, &status)
        .expect_err("the daemon commit component must match exactly");
    assert!(error.message.contains("expected commit `06bbf46013cf`"));
}

fn disconnect_report(disconnected: bool, partial: bool) -> crate::RunnerDisconnectReport {
    crate::RunnerDisconnectReport {
        runner_id: "lab".to_string(),
        disconnected,
        partial,
        remote_error: partial.then(|| "remote daemon stop was not proven".to_string()),
        local_recovery_command: partial
            .then(|| "homeboy runner disconnect lab --local-recovery".to_string()),
        session: None,
        session_path: String::new(),
    }
}

/// A partial disconnect retains the session and the old daemon. It must fail
/// the `disconnect` phase instead of letting refresh reconnect onto that daemon.
#[test]
fn partial_disconnect_fails_the_disconnect_phase_with_the_remote_error() {
    let session = refreshed_daemon_status(true, Some("homeboy 0.417.15+oldcommit"))
        .session
        .expect("connected status carries a session");

    let error = disconnect_before_reconnect(Some(&session), |_| {
        require_completed_disconnect("lab", &disconnect_report(false, true))
    })
    .expect_err("a partial disconnect did not retire the old daemon");

    assert!(error.message.contains("did not retire the previous daemon"));
    assert!(error.message.contains("remote daemon stop was not proven"));

    disconnect_before_reconnect(Some(&session), |_| {
        require_completed_disconnect("lab", &disconnect_report(true, false))
    })
    .expect("a completed disconnect lets the refresh proceed to reconnect");
}

/// Reattaching the pre-refresh lease must not certify the daemon refresh, even
/// when the session reports the refreshed build identity.
#[test]
fn refreshed_daemon_verification_rejects_reattaching_the_pre_refresh_lease() {
    let mut reattached = refreshed_daemon_status(true, Some("homeboy 0.417.15+19a41cd5102d"));
    reattached
        .session
        .as_mut()
        .expect("session")
        .remote_daemon_lease_id = Some("lease-old".to_string());

    let error = verify_refreshed_daemon_topology_with(
        "lab",
        "19a41cd5102d",
        Some("lease-old"),
        || Ok(reattached.clone()),
        || std::thread::sleep(Duration::from_millis(50)),
    );
    // The retry loop ends once the verification window elapses; the failure
    // must name the retained lease.
    let error = error.expect_err("the old lease was reattached, not replaced");
    assert!(error
        .message
        .contains("pre-refresh daemon lease `lease-old`"));

    let mut replaced = reattached;
    replaced
        .session
        .as_mut()
        .expect("session")
        .remote_daemon_lease_id = Some("lease-new".to_string());
    verify_refreshed_daemon_topology_status("lab", "19a41cd5102d", Some("lease-old"), &replaced)
        .expect("a new lease on the refreshed binary converges");
}

#[test]
fn select_plan_only_probes_requested_binary() {
    let script = identity_probe_script("/opt/homeboy/bin/homeboy");

    assert!(script.contains("'/opt/homeboy/bin/homeboy' self identity"));
    assert!(!script.contains("cargo build"));
}

#[test]
fn select_without_materialization_sha_promotes_the_verified_binary() {
    crate::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let mut plan = ssh_bootstrap_plan();
        plan.mode = "select".to_string();
        plan.binary_path = "/selected/homeboy".to_string();

        let promoted = ssh_bootstrap_promote_with(
            &plan,
            || Ok(r#"{"data":{"git_commit":"abc123","git_dirty":false}}"#.to_string()),
            |path, _| {
                let patch = refreshed_runner_patch_in_roots("lab-local", path)?;
                match merge(Some("lab-local"), &patch.to_string(), &[])? {
                    MergeOutput::Single(result) => Ok((result.updated_fields, None)),
                    MergeOutput::Bulk(_) => Ok((Vec::new(), None)),
                }
            },
        )
        .expect("select has no materialization SHA requirement");

        assert_eq!(promoted.source_sha, None);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/selected/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_only_its_own_selected_binary() {
    crate::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/newer/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected_in_roots(
            &ambient_roots(),
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(!restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/newer/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_its_own_selected_binary() {
    crate::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/selected/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected_in_roots(
            &ambient_roots(),
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn post_promotion_active_job_race_restores_prior_selection_without_stopping_daemon() {
    crate::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/selected/homeboy"}"#,
            false,
        )
        .expect("runner");

        let deferred = defer_reconnect_after_promotion_race_in_roots(
            &ambient_roots(),
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
            &[active_admission("job-raced-after-precheck")],
        )
        .expect("defer raced reconnect");

        assert_eq!(deferred.reason, "active_daemon_jobs");
        assert_eq!(deferred.active_job_ids, ["job-raced-after-precheck"]);
        assert!(deferred.ownership_contention.is_none());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn post_promotion_active_job_race_preserves_newer_selector_as_contention() {
    crate::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/newer/homeboy"}"#,
            false,
        )
        .expect("runner");

        let deferred = defer_reconnect_after_promotion_race_in_roots(
            &ambient_roots(),
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
            &[active_admission("job-raced-after-precheck")],
        )
        .expect("defer raced reconnect");

        assert!(deferred
            .ownership_contention
            .as_deref()
            .is_some_and(|message| message.contains("preserving the newer owner")));
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/newer/homeboy")
        );
    });
}

#[test]
fn rollback_failure_keeps_the_primary_refresh_error() {
    let error = rollback_refresh_error_with::<(), _>(
        Error::validation_invalid_argument("disconnect", "primary stop failure", None, None),
        || {
            Err(Error::internal_io(
                "rollback write failure".to_string(),
                None,
            ))
        },
    )
    .expect_err("rollback failure is surfaced with the primary failure");

    assert!(error.message.contains("primary stop failure"));
    assert!(error.message.contains("rollback write failure"));
    assert_eq!(
        error.details["rollback_error"]["details"]["error"],
        "rollback write failure"
    );
}

#[test]
fn rollback_failure_persists_one_exact_previous_binary_continuation() {
    crate::test_support::with_isolated_home(|_| {
        let error = durable_refresh_partial_error(
            Error::internal_unexpected("rollback reconnect failed"),
            "lab runner",
            Some("/stable builds/homeboy"),
            true,
        );
        let path = error.details["partial_state_path"]
            .as_str()
            .expect("partial state path");
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("durable partial state"))
                .expect("partial state JSON");

        assert_eq!(state["status"], "rollback_incomplete");
        assert_eq!(state["previous_binary_path"], "/stable builds/homeboy");
        assert_eq!(
            state["continuation_command"],
            "homeboy runner refresh-homeboy 'lab runner' --select '/stable builds/homeboy' --reconnect --force"
        );
        assert_eq!(
            error.details["continuation_command"],
            state["continuation_command"]
        );
    });
}

#[test]
fn successful_rollback_does_not_publish_partial_state() {
    crate::test_support::with_isolated_home(|_| {
        let primary = rollback_refresh_error_with::<(), _>(
            Error::internal_unexpected("candidate connect failed"),
            || Ok(()),
        )
        .expect_err("primary failure remains visible");
        let error =
            durable_refresh_partial_error_if_needed(primary, "lab", Some("/stable/homeboy"), false);

        assert!(error.details.get("partial_state").is_none());
        assert!(!refresh_partial_state_path("lab")
            .expect("partial path")
            .exists());
    });
}

#[test]
fn release_slots_live_under_the_workspace_binaries_dir() {
    let binaries = release_binaries_dir("/runner/ws/");
    assert_eq!(binaries, "/runner/ws/_homeboy_binaries");
    let script = materialize_script("v1.2.3", &binaries);
    assert!(script.contains(&format!("binaries={}\n", quote_path(&binaries))));
    assert!(script.contains("slot_dir=\"$binaries/homeboy-$binary_sha\""));
    assert!(script.contains("release_marker=\"$binaries/.release-$release_tag.sha256\""));
    assert!(
        !script.contains("@SOURCE@") && !script.contains("@DIR@") && !script.contains("@BINARY@")
    );
}

#[test]
fn parse_identity_reads_final_pretty_json_after_command_output() {
    let identity = parse_identity(
        "HEAD is now at abc123 fix runner\n{\n  \"success\": true,\n  \"data\": {\n    \"version\": \"0.263.0\"\n  }\n}\n",
    )
    .expect("identity parses");

    assert_eq!(identity["data"]["version"], "0.263.0");
}

#[test]
fn disconnected_ssh_refresh_dispatches_the_existing_script_with_bounded_transport() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        git_ref: Some("accepted-sha".to_string()),
        binary_path: "/runner/_homeboy_binaries".to_string(),
        script: "managed clone fetch build select".to_string(),
        reconnect: true,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(
        &plan,
        vec!["bash".to_string(), "git".to_string(), "cargo".to_string()],
        true,
    );

    assert!(options.allow_diagnostic_ssh);
    assert_eq!(
        options.diagnostic_ssh_timeout,
        Some(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    );
    assert_eq!(
        options.command,
        vec!["bash", "-lc", "managed clone fetch build select"]
    );
    assert!(options
        .capability_preflight
        .expect("bootstrap preflight")
        .required_commands
        .is_empty());
}

fn stale_daemon_admission_snapshot(
    active_job_count: usize,
    lease_id: Option<&str>,
    pid: Option<u32>,
) -> RunnerAdmissionSnapshot {
    let mut status = refreshed_daemon_status(true, Some("homeboy 0.1.0+stale"));
    status.stale_daemon = Some(
        RunnerStaleDaemonWarning::new(
            "lab",
            "old".to_string(),
            "current".to_string(),
            None,
            Some("current".to_string()),
        )
        .with_runtime_paths(
            "lab",
            Vec::new(),
            vec![crate::RunnerChangedRuntimePath {
                env: "HOMEBOY_PLUGIN_PATH".to_string(),
                loaded_path: Some("/old".to_string()),
                configured_path: Some("/new".to_string()),
            }],
        ),
    );
    status.daemon_freshness = Some(DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
        restartable: true,
        lease_id: lease_id.map(str::to_string),
        pid,
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: active_job_count,
        termination_evidence: None,
        repair_plan: Vec::new(),
    });
    status.active_job_count = active_job_count;
    status.active_job_state = RunnerActiveJobState::Available;
    RunnerAdmissionSnapshot::from_status(status)
}

#[test]
fn refresh_routing_preserves_disconnected_ssh_execution_and_fences_unsafe_stale_daemons() {
    let runner = crate::Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: None,
        workspace_root: None,
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };
    let disconnected = RunnerAdmissionSnapshot::from_status(refreshed_daemon_status(false, None));

    let route = refresh_execution_route(&runner, &disconnected)
        .expect("disconnected SSH refresh retains diagnostic execution");
    let options = refresh_execution_options(
        &ssh_bootstrap_plan(),
        vec!["bash".to_string()],
        route.uses_diagnostic_ssh(),
    );
    assert!(options.allow_diagnostic_ssh);

    let unsafe_stale = stale_daemon_admission_snapshot(1, Some("lease-current"), Some(1));
    let error = refresh_execution_route(&runner, &unsafe_stale)
        .expect_err("connected stale daemon with active work remains fenced");
    assert_eq!(error.code.as_str(), "validation.invalid_argument");
    assert!(error.message.contains("permits daemon rotation"));
    // #15995: the fence names the jobs, and its hint leads somewhere other than
    // the refresh it just refused.
    assert!(
        error.message.contains("1 live job(s)")
            && error.message.contains("homeboy runner job cancel lab"),
        "{}",
        error.message
    );
    assert!(!error
        .hints
        .iter()
        .any(|hint| hint.message.contains("refresh-homeboy")));

    let ambiguous_stale = stale_daemon_admission_snapshot(0, None, None);
    assert!(refresh_execution_route(&runner, &ambiguous_stale).is_err());
}

#[test]
fn reconcile_then_refresh_uses_the_idle_postcondition() {
    let runner = crate::Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: None,
        workspace_root: None,
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };
    let reconciled = stale_daemon_admission_snapshot(0, Some("lease-current"), Some(1)).status;
    let operations = RefCell::new(Vec::new());

    let admission = reconciled_refresh_admission_with(
        "lab",
        |_| {
            operations.borrow_mut().push("reconcile");
            Ok(reconciled)
        },
        |status| {
            operations.borrow_mut().push("project");
            RunnerAdmissionSnapshot::from_status(status)
        },
    )
    .expect("reconciled refresh admission");

    assert_eq!(operations.into_inner(), ["reconcile", "project"]);
    assert!(admission.summary.safe_to_rotate);
    assert_eq!(
        refresh_execution_route(&runner, &admission).expect("idle stale daemon routes over SSH"),
        RefreshExecutionRoute::DiagnosticSsh
    );
}

#[test]
fn reconcile_then_refresh_fails_closed_when_the_postcondition_has_a_live_job() {
    let runner = crate::Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: None,
        workspace_root: None,
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };
    let live = stale_daemon_admission_snapshot(1, Some("lease-current"), Some(1)).status;

    let admission = reconciled_refresh_admission_with(
        "lab",
        |_| Ok(live),
        RunnerAdmissionSnapshot::from_status,
    )
    .expect("live postcondition projects");

    assert!(!admission.summary.safe_to_rotate);
    assert!(refresh_execution_route(&runner, &admission).is_err());
}

#[test]
fn connected_refresh_keeps_daemon_execution_options() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        git_ref: None,
        binary_path: "/runner/homeboy".to_string(),
        script: "probe".to_string(),
        reconnect: false,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(&plan, vec!["bash".to_string()], false);

    assert!(!options.allow_diagnostic_ssh);
    assert_eq!(options.diagnostic_ssh_timeout, None);
    assert!(options.mirror_evidence);
    assert!(options.print_handoff);
}

#[test]
fn terminal_refresh_errors_include_the_completed_and_failed_phases() {
    for phase in ["restart_in_place", "disconnect", "reconnect_transport"] {
        let error = refresh_error_with_phase_summary(
            Error::internal_unexpected("terminal refresh failure"),
            &[
                refresh_phase("materialize", true, 0),
                refresh_phase(phase, true, 1),
            ],
        );
        assert_eq!(error.details["phase_summary"][1]["name"], phase);
        assert_eq!(error.details["phase_summary"][1]["status"], "failed");
    }
}
