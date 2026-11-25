#![cfg(test)]

use super::*;
use crate::{
    RunnerActiveJobState, RunnerAdmissionSnapshot, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStatusReport,
};
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use std::cell::RefCell;

// The restart-in-place harness drives Linux process supervision.
#[cfg(target_os = "linux")]
#[test]
fn refresh_with_active_jobs_restarts_in_place_and_preserves_job_ids() {
    crate::execution::tests::restart_in_place::refresh_with_active_jobs_keeps_successor_ownership();
}

fn controller_session(role: RunnerSessionRole, mode: RunnerTunnelMode) -> RunnerSession {
    RunnerSession {
        machine_id: None,
        runner_id: "lab".to_string(),
        mode,
        role,
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
    }
}

#[test]
fn refresh_preserves_only_its_direct_controller_lease_for_orphan_recovery() {
    for (role, mode, expected) in [
        (
            RunnerSessionRole::Controller,
            RunnerTunnelMode::DirectSsh,
            Some("lease-refresh"),
        ),
        (RunnerSessionRole::Runner, RunnerTunnelMode::DirectSsh, None),
        (
            RunnerSessionRole::Controller,
            RunnerTunnelMode::Loopback,
            None,
        ),
    ] {
        assert_eq!(
            refresh_owned_lease(controller_session(role, mode)).as_deref(),
            expected
        );
    }
}

#[test]
fn refresh_rotates_only_a_connected_daemon_before_connecting_the_promoted_binary() {
    disconnect_before_reconnect(None, |_| {
        panic!("a disconnected runner has no daemon to rotate")
    })
    .expect("disconnected refresh does not attempt daemon rotation");

    let session = controller_session(RunnerSessionRole::Controller, RunnerTunnelMode::DirectSsh);
    let rotated = RefCell::new(Vec::new());
    disconnect_before_reconnect(Some(&session), |session| {
        rotated
            .borrow_mut()
            .push(session.remote_daemon_lease_id.clone());
        Ok(())
    })
    .expect("connected refresh rotates the existing daemon");
    assert_eq!(rotated.into_inner(), [Some("lease-refresh".to_string())]);
}

/// #12418: materialization begins from a disconnected observation, but another
/// controller can connect the old daemon before this refresh obtains promotion
/// ownership. The post-lease session must drive retirement; a retained stale
/// record for a disconnected runner must not block connecting the promotion.
#[test]
fn materialization_race_restarts_the_newly_connected_daemon_and_preserves_active_work() {
    let mut materialization_snapshot = refreshed_daemon_status(false, None);
    materialization_snapshot.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+newcommit".to_string()),
    ));
    let mut post_lease_status = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    let old_session = post_lease_status
        .session
        .as_mut()
        .expect("post-materialization connection persists a session");
    old_session.remote_daemon_lease_id = Some("lease-old".to_string());
    old_session.remote_daemon_pid = Some(42);

    assert!(
        reconnect_session_after_promotion(&materialization_snapshot).is_none(),
        "materialization began while the runner was disconnected"
    );
    let post_lease_session = reconnect_session_after_promotion(&post_lease_status)
        .expect("the post-lease connected daemon is the one to retire");
    assert_eq!(
        post_lease_session.remote_daemon_lease_id.as_deref(),
        Some("lease-old")
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
            machine_id: None,
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
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
        recent_observation_failure: None,
        session_path: String::new(),
    }
}

#[test]
fn refreshed_daemon_verification_retries_until_daemon_and_configured_binary_converge() {
    let converged = || refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    // Post-start health window: the tunnel is not ready on the first probe.
    let not_ready = refreshed_daemon_status(false, Some("homeboy 0.294.0+19a41cd5102d"));
    // The daemon still reports the pre-refresh build.
    let mut stale_daemon = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale_daemon.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+19a41cd5102d".to_string()),
    ));
    // The daemon matches but the configured job binary is still stale.
    let mut stale_configured = converged();
    stale_configured.configured_job_binary_build_identity =
        Some("homeboy 0.294.0+oldcommit".to_string());

    for first in [not_ready, stale_daemon, stale_configured] {
        let mut statuses = [first, converged()].into_iter();
        let mut retries = 0;
        verify_refreshed_daemon_topology_with(
            "lab",
            "19a41cd5102d",
            None,
            || Ok(statuses.next().expect("topology status probe")),
            || retries += 1,
        )
        .expect("refresh converges once daemon and configured executable identities match");
        assert_eq!(retries, 1);
    }
}

#[test]
fn refreshed_daemon_verification_rejects_mismatched_identities() {
    let mut configured_mismatch = refreshed_daemon_status(true, Some("homeboy 0.1.0+06bbf46013cf"));
    configured_mismatch.configured_job_binary_build_identity =
        Some("homeboy 0.1.0+oldcommit".to_string());
    let substring_mismatch = refreshed_daemon_status(true, Some("homeboy 0.1.0+x06bbf46013cf"));

    for (status, expected) in [
        (configured_mismatch, "retained configured job binary"),
        (substring_mismatch, "expected commit `06bbf46013cf`"),
    ] {
        let error = verify_refreshed_daemon_topology_status("lab", "06bbf46013cf", None, &status)
            .expect_err("mismatched topology must not certify refresh success");
        assert!(error.message.contains(expected), "{}", error.message);
    }
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
    for (configured, expected_restored, expected_path) in [
        ("/selected/homeboy", true, "/stable/homeboy"),
        ("/newer/homeboy", false, "/newer/homeboy"),
    ] {
        crate::test_support::with_isolated_home(|_| {
            crate::create(
                &serde_json::json!({"id": "lab-local", "kind": "local", "homeboy_path": configured})
                    .to_string(),
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

            assert_eq!(restored, expected_restored, "{configured}");
            assert_eq!(
                crate::load("lab-local")
                    .expect("reload")
                    .settings
                    .homeboy_path
                    .as_deref(),
                Some(expected_path)
            );
        });
    }
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
            "homeboy runner refresh-homeboy 'lab runner' --select '/stable builds/homeboy'"
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
            durable_refresh_partial_error_if_needed(primary, "lab", Some("/stable/homeboy"));

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
fn refresh_execution_options_bound_disconnected_ssh_and_keep_daemon_admission_when_connected() {
    let disconnected = refresh_execution_options(
        "managed clone fetch build select",
        vec!["bash".to_string(), "git".to_string(), "cargo".to_string()],
        true,
    );
    assert!(disconnected.allow_diagnostic_ssh);
    assert_eq!(
        disconnected.diagnostic_ssh_timeout,
        Some(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    );
    assert_eq!(
        disconnected.command,
        vec!["bash", "-lc", "managed clone fetch build select"]
    );
    assert!(disconnected
        .capability_preflight
        .expect("bootstrap preflight")
        .required_commands
        .is_empty());

    let connected = refresh_execution_options("probe", vec!["bash".to_string()], false);
    assert!(!connected.allow_diagnostic_ssh);
    assert_eq!(connected.diagnostic_ssh_timeout, None);
    assert!(connected.mirror_evidence);
    assert!(connected.print_handoff);
    // #18376: the daemon admits the refresh outside bulk pressure admission.
    assert_eq!(
        connected
            .env
            .get(homeboy_core::runner_pressure::ADMISSION_CLASS_ENV)
            .map(String::as_str),
        Some(homeboy_core::runner_pressure::CONVERGENCE_ADMISSION_CLASS)
    );
}

fn ssh_runner() -> crate::Runner {
    crate::Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: None,
        workspace_root: None,
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    }
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
        stale_reason_code: Some(DaemonStaleReasonCode::LabContractVersionMismatch),
        restartable: true,
        lease_id: lease_id.map(str::to_string),
        pid,
        recovery_evidence: None,
        ownership_evidence: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: active_job_count,
        termination_evidence: None,
        repair_plan: Vec::new(),
    });
    status.active_job_count = active_job_count;
    status.active_job_state = RunnerActiveJobState::Available;
    crate::runner_admission_snapshot_for_status(
        status,
        crate::RunnerAdmissionReadiness::Observation,
    )
}

#[test]
fn refresh_routing_preserves_disconnected_ssh_execution_and_fences_unsafe_stale_daemons() {
    let runner = ssh_runner();
    let disconnected = crate::runner_admission_snapshot_for_status(
        refreshed_daemon_status(false, None),
        crate::RunnerAdmissionReadiness::Observation,
    );

    let route = refresh_execution_route(&runner, &disconnected)
        .expect("disconnected SSH refresh retains diagnostic execution");
    assert!(route.uses_diagnostic_ssh());

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
fn reconcile_then_refresh_routes_on_the_reconciled_postcondition() {
    for (active_jobs, safe_to_rotate) in [(0, true), (1, false)] {
        let reconciled =
            stale_daemon_admission_snapshot(active_jobs, Some("lease-current"), Some(1)).status;
        let operations = RefCell::new(Vec::new());

        let admission = reconciled_refresh_admission_with(
            "lab",
            |_| {
                operations.borrow_mut().push("reconcile");
                Ok(reconciled)
            },
            |status| {
                operations.borrow_mut().push("project");
                crate::runner_admission_snapshot_for_status(
                    status,
                    crate::RunnerAdmissionReadiness::Observation,
                )
            },
        )
        .expect("reconciled refresh admission");

        assert_eq!(operations.into_inner(), ["reconcile", "project"]);
        assert_eq!(admission.summary.safe_to_rotate, safe_to_rotate);
        let route = refresh_execution_route(&ssh_runner(), &admission);
        if safe_to_rotate {
            assert_eq!(
                route.expect("idle stale daemon routes over SSH"),
                RefreshExecutionRoute::DiagnosticSsh
            );
        } else {
            assert!(route.is_err(), "a live job fails closed");
        }
    }
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
